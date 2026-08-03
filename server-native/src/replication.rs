//! Replication: the listener that serves replicas, the client that follows a
//! primary, and the authentication throttle guarding the handshake.

use crate::*;

pub(crate) type ReplSender = mpsc::Sender<Vec<u8>>;

/// A connected replica: its write channel plus the counters that make lag
/// observable.
///
/// Replication was previously one-way, so the primary could only report how
/// many replicas were attached — never how far behind one had fallen. The
/// replica now acknowledges each applied frame, and the difference between
/// what was queued and what was acknowledged is the lag.
pub(crate) struct ReplicaHandle {
    pub(crate) tx: ReplSender,
    /// Frames handed to this replica's channel.
    pub(crate) sent: Arc<AtomicU64>,
    /// Frames the replica reports as applied.
    pub(crate) acked: Arc<AtomicU64>,
}

/// Connected-replica registry. `count` mirrors `senders.len()` (updated by
/// every writer while holding the lock) so the per-write hot path can skip
/// the mutex entirely when no replica is connected.
pub(crate) struct ReplHub {
    pub(crate) senders: tokio::sync::Mutex<Vec<ReplicaHandle>>,
    pub(crate) count: AtomicUsize,
}

impl ReplHub {
    /// Deepest send queue across connected replicas, in frames.
    ///
    /// Replication is fire-and-forget — replicas never acknowledge an applied
    /// offset — so true offset lag is not observable without a protocol change.
    /// Queue depth is the honest proxy available today: a replica that cannot
    /// keep up backs its channel up, and a queue at capacity means frames are
    /// about to be dropped.
    pub(crate) async fn max_queue_depth(&self) -> usize {
        let senders = self.senders.lock().await;
        senders
            .iter()
            .map(|r| r.tx.max_capacity().saturating_sub(r.tx.capacity()))
            .max()
            .unwrap_or(0)
    }

    /// Send one frame to every attached replica, dropping any that cannot keep
    /// up. Increments each surviving replica's sent counter, which is one half
    /// of the lag calculation.
    pub(crate) async fn fan_out(&self, bytes: Vec<u8>) {
        let mut reg = self.senders.lock().await;
        reg.retain(|r| match r.tx.try_send(bytes.clone()) {
            Ok(()) => {
                r.sent.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!(
                    "Replica fell too far behind (channel full) — disconnecting so it can resync"
                );
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        });
        self.count.store(reg.len(), Ordering::Relaxed);
    }

    /// Frames the furthest-behind replica has yet to acknowledge.
    ///
    /// This is true lag: how much of what the primary sent has actually been
    /// applied downstream. Queue depth only shows what is stuck locally, and
    /// reads zero for a replica that has received frames but cannot apply them.
    pub(crate) async fn max_lag_frames(&self) -> u64 {
        let senders = self.senders.lock().await;
        senders
            .iter()
            .map(|r| {
                r.sent
                    .load(Ordering::Relaxed)
                    .saturating_sub(r.acked.load(Ordering::Relaxed))
            })
            .max()
            .unwrap_or(0)
    }

    pub(crate) fn new() -> ReplRegistry {
        Arc::new(ReplHub {
            senders: tokio::sync::Mutex::new(Vec::new()),
            count: AtomicUsize::new(0),
        })
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.count.load(Ordering::Relaxed) == 0
    }
}

pub(crate) type ReplRegistry = Arc<ReplHub>;

/// Default per-replica channel capacity (number of pending write frames).
/// When a replica falls this many writes behind the primary it is disconnected
/// so it can reconnect and receive a fresh snapshot — the primary write path
/// is never blocked.
pub(crate) const DEFAULT_REPL_CHANNEL_CAPACITY: usize = 4096;

/// Upper bound on a single length-prefixed replication frame (snapshot or
/// command). The replication port may be unauthenticated and plaintext, so an
/// untrusted peer could otherwise send a 4 GB length prefix and force a matching
/// allocation. 512 MB comfortably covers a large snapshot while bounding abuse.
pub(crate) const MAX_REPL_FRAME_BYTES: usize = 512 * 1024 * 1024;

// ── Server state ──────────────────────────────────────────────────────────────

/// Per-peer replication auth throttle.
///
/// The RESP port drops a connection after `MAX_AUTH_FAILURES` guesses, but the
/// replication handshake is one-shot: a wrong password costs the attacker a
/// single TCP connection and nothing else, so the port offered effectively
/// unlimited guesses at a secret that yields the entire keyspace. Failures are
/// counted per source address over a rolling window, and a peer that exhausts
/// them is refused before the handshake is read at all.
///
/// Keyed by address rather than by connection, which is the whole point — the
/// weakness being closed is that reconnecting reset the count.
pub(crate) struct ReplAuthThrottle {
    pub(crate) failures: std::sync::Mutex<HashMap<IpAddr, (u32, std::time::Instant)>>,
}

impl ReplAuthThrottle {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            failures: std::sync::Mutex::new(HashMap::new()),
        })
    }

    /// True when this peer has spent its attempts and must be refused.
    pub(crate) fn is_blocked(&self, ip: IpAddr) -> bool {
        let Ok(map) = self.failures.lock() else {
            return false;
        };
        match map.get(&ip) {
            Some((count, last)) => *count >= MAX_AUTH_FAILURES && last.elapsed() < REPL_AUTH_WINDOW,
            None => false,
        }
    }

    pub(crate) fn record_failure(&self, ip: IpAddr) {
        let Ok(mut map) = self.failures.lock() else {
            return;
        };
        let now = std::time::Instant::now();
        // Sweep before inserting so a spray across many source addresses cannot
        // grow the map without bound.
        if map.len() >= REPL_AUTH_SWEEP_THRESHOLD {
            map.retain(|_, (_, last)| last.elapsed() < REPL_AUTH_WINDOW);
        }
        let entry = map.entry(ip).or_insert((0, now));
        // A peer that went quiet for longer than the window starts over, so a
        // slow trickle is not punished forever.
        if entry.1.elapsed() >= REPL_AUTH_WINDOW {
            *entry = (0, now);
        }
        entry.0 = entry.0.saturating_add(1);
        entry.1 = now;
    }

    pub(crate) fn record_success(&self, ip: IpAddr) {
        if let Ok(mut map) = self.failures.lock() {
            map.remove(&ip);
        }
    }
}

/// Read the newline-terminated replication auth line.
///
/// Reads until the terminator rather than reading exactly `password.len() + 1`
/// bytes, which is how the previous implementation worked: the number of bytes
/// the server waited for *was* the password length, so an attacker could
/// recover it exactly by drip-feeding one byte at a time and watching when the
/// server replied.
pub(crate) async fn read_repl_auth_line<S>(socket: &mut S) -> std::io::Result<Vec<u8>>
where
    S: AsyncRead + Unpin,
{
    let mut line = Vec::with_capacity(64);
    let mut byte = [0u8; 1];
    loop {
        socket.read_exact(&mut byte).await?;
        if byte[0] == b'\n' {
            return Ok(line);
        }
        if line.len() >= MAX_REPL_AUTH_LINE {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "replication auth line too long",
            ));
        }
        line.push(byte[0]);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_repl_server(
    bind_host: String,
    port: u16,
    store: Arc<KeyValueStore>,
    snap_cfg: Arc<SnapshotConfig>,
    replicas: ReplRegistry,
    repl_password: Option<Arc<String>>,
    repl_channel_capacity: usize,
    allowed_ips: Option<Arc<Vec<IpAddr>>>,
    semaphore: Arc<Semaphore>,
    throttle: Arc<ReplAuthThrottle>,
    tls: Arc<Option<TlsAcceptor>>,
) {
    let listener = match TcpListener::bind(format!("{}:{}", bind_host, port)).await {
        Ok(l) => l,
        Err(e) => {
            warn!("Replication listener failed to bind :{}: {}", port, e);
            return;
        }
    };
    info!(
        "Replication server listening on {}:{} ({})",
        bind_host,
        port,
        if tls.is_some() { "TLS" } else { "plaintext" }
    );
    loop {
        match listener.accept().await {
            Ok((socket, addr)) => {
                // The IP allowlist and the connection limit were previously
                // applied on the RESP and WebSocket listeners only, so neither
                // constrained the one port that streams the whole keyspace.
                if let Some(allowed) = &allowed_ips
                    && !allowed.contains(&addr.ip())
                {
                    debug!("Replication: rejected IP {}", addr.ip());
                    continue;
                }
                if throttle.is_blocked(addr.ip()) {
                    warn!(
                        "Replication: {} refused — too many failed auth attempts",
                        addr.ip()
                    );
                    continue;
                }
                let permit = match Arc::clone(&semaphore).try_acquire_owned() {
                    Ok(p) => p,
                    Err(_) => {
                        warn!("Replication: connection limit reached, dropping {}", addr);
                        continue;
                    }
                };
                info!("Replica connected from {}", addr);
                let store = Arc::clone(&store);
                let snap_cfg = Arc::clone(&snap_cfg);
                let replicas = Arc::clone(&replicas);
                let pwd = repl_password.clone();
                let thr = Arc::clone(&throttle);
                let tls = Arc::clone(&tls);
                tokio::spawn(async move {
                    let _permit = permit;
                    // Bounded like the other listeners: the permit is already
                    // held, so a peer that never negotiates must not keep it.
                    let outcome = if let Some(acceptor) = tls.as_ref() {
                        match tokio::time::timeout(handshake_timeout(), acceptor.accept(socket))
                            .await
                        {
                            Ok(Ok(stream)) => {
                                handle_replica(
                                    stream,
                                    store,
                                    snap_cfg,
                                    replicas,
                                    pwd,
                                    repl_channel_capacity,
                                    addr.ip(),
                                    thr,
                                )
                                .await
                            }
                            Ok(Err(e)) => {
                                // The most likely cause by far is a replica that
                                // has not been given RECACHED_REPL_TLS_CA, which
                                // otherwise looks like an unexplained disconnect.
                                warn!(
                                    "Replication TLS handshake failed from {}: {} — is that \
                                     replica configured with RECACHED_REPL_TLS_CA?",
                                    addr, e
                                );
                                return;
                            }
                            Err(_) => {
                                debug!("Replication TLS handshake from {} timed out", addr);
                                return;
                            }
                        }
                    } else {
                        handle_replica(
                            socket,
                            store,
                            snap_cfg,
                            replicas,
                            pwd,
                            repl_channel_capacity,
                            addr.ip(),
                            thr,
                        )
                        .await
                    };
                    if let Err(e) = outcome {
                        info!("Replica {} disconnected: {}", addr, e);
                    }
                });
            }
            Err(e) => warn!("Replication accept error: {}", e),
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_replica<S>(
    mut socket: S,
    store: Arc<KeyValueStore>,
    _snap_cfg: Arc<SnapshotConfig>,
    replicas: ReplRegistry,
    repl_password: Option<Arc<String>>,
    repl_channel_capacity: usize,
    peer_ip: IpAddr,
    throttle: Arc<ReplAuthThrottle>,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    // 0. Auth handshake — replica must send "<password>\n" before anything else.
    //
    // Bounded by a deadline as well as a length: a peer that connects and then
    // says nothing would otherwise hold its connection permit forever.
    if let Some(pwd) = &repl_password {
        let line = match tokio::time::timeout(handshake_timeout(), read_repl_auth_line(&mut socket))
            .await
        {
            Ok(res) => res?,
            Err(_) => {
                return Err(std::io::Error::new(
                    ErrorKind::TimedOut,
                    "replication auth handshake timed out",
                ));
            }
        };
        if !ct_eq_bytes(&line, pwd.as_bytes()) {
            throttle.record_failure(peer_ip);
            let _ = socket
                .write_all(b"-ERR invalid replication password\n")
                .await;
            return Err(std::io::Error::new(
                ErrorKind::PermissionDenied,
                "replication auth failed",
            ));
        }
        throttle.record_success(peer_ip);
        socket.write_all(b"+OK\n").await?;
        socket.flush().await?;
    }

    // 1. Register channel first so subsequent writes are buffered
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(repl_channel_capacity);
    let sent = Arc::new(AtomicU64::new(0));
    let acked = Arc::new(AtomicU64::new(0));
    {
        let mut reg = replicas.senders.lock().await;
        reg.push(ReplicaHandle {
            tx,
            sent: Arc::clone(&sent),
            acked: Arc::clone(&acked),
        });
        replicas.count.store(reg.len(), Ordering::Relaxed);
    }

    // 2. Take snapshot and send (writes since snapshot are in channel)
    let snap_bytes =
        rmp_serde::to_vec(&store.snapshot()).map_err(|e| std::io::Error::other(e.to_string()))?;
    let len = snap_bytes.len() as u32;
    socket.write_all(&len.to_le_bytes()).await?;
    socket.write_all(&snap_bytes).await?;
    socket.flush().await?;

    // 3. Stream buffered + ongoing writes, and read acknowledgements
    //
    // The socket is bidirectional but used to carry frames one way only, which
    // left the primary unable to say how far behind a replica was. The replica
    // now writes back a cumulative count of applied frames; `sent - acked` is
    // the lag. Reading and writing are selected over so a replica that stops
    // acknowledging cannot stall the write side, and vice versa.
    let (mut rd, mut wr) = tokio::io::split(socket);
    let mut ack_buf = [0u8; 8];
    loop {
        tokio::select! {
            frame = rx.recv() => {
                let Some(bytes) = frame else { break };
                let len = bytes.len() as u32;
                wr.write_all(&len.to_le_bytes()).await?;
                wr.write_all(&bytes).await?;
                wr.flush().await?;
            }
            res = rd.read_exact(&mut ack_buf) => {
                // A replica that closes its read side, or one running a build
                // that predates acks, simply stops updating the gauge — it is
                // not an error, so the stream continues either way.
                if res.is_err() {
                    break;
                }
                let applied = u64::from_le_bytes(ack_buf);
                // Monotonic: a reordered or replayed ack must never walk the
                // high-water mark backwards and report negative lag.
                acked.fetch_max(applied, Ordering::Relaxed);
            }
        }
    }
    Ok(())
}

// ── Replication client (replica side) ────────────────────────────────────────

pub(crate) async fn run_repl_client(
    primary_addr: String,
    store: Arc<KeyValueStore>,
    state: Arc<ServerState>,
    repl_password: Option<String>,
    failover_timeout_secs: Option<u64>,
    tx: broadcast::Sender<SyncMsg>,
    tls: Option<(TlsConnector, String)>,
) {
    let mut backoff_secs = 2u64;
    let mut unreachable_since: Option<std::time::Instant> = None;

    loop {
        // Stop if already promoted (manual REPLICAOF NO ONE or earlier auto-promotion).
        if !state.is_replica() {
            return;
        }

        info!("Replica: connecting to primary at {}", primary_addr);
        match TcpStream::connect(&primary_addr).await {
            Err(e) => {
                warn!("Replica: connect failed: {}", e);
                unreachable_since.get_or_insert_with(std::time::Instant::now);
            }
            Ok(socket) => {
                // Primary is reachable — reset the unreachable timer.
                unreachable_since = None;
                backoff_secs = 2;

                // TLS is what makes the primary's *identity* checked, not just
                // the channel encrypted: without it a DNS hijack or an on-path
                // attacker can feed this replica an arbitrary keyspace, and the
                // replica has no way to tell.
                let result = match &tls {
                    None => {
                        sync_from_primary(
                            &mut { socket },
                            &store,
                            repl_password.as_deref(),
                            &tx,
                            &state,
                        )
                        .await
                    }
                    Some((connector, servername)) => {
                        match ServerName::try_from(servername.clone()) {
                            Err(_) => {
                                error!(
                                    "Replica: '{}' is not a valid TLS server name — set \
                                     RECACHED_REPL_TLS_SERVERNAME to the name on the primary's \
                                     certificate",
                                    servername
                                );
                                return;
                            }
                            Ok(name) => {
                                match tokio::time::timeout(
                                    handshake_timeout(),
                                    connector.connect(name, socket),
                                )
                                .await
                                {
                                    Err(_) => Err(std::io::Error::new(
                                        ErrorKind::TimedOut,
                                        "TLS handshake with primary timed out",
                                    )),
                                    Ok(Err(e)) => Err(std::io::Error::other(format!(
                                        "TLS handshake with primary failed: {e} — check that the \
                                         primary has RECACHED_TLS_CERT set and that \
                                         RECACHED_REPL_TLS_CA trusts it"
                                    ))),
                                    Ok(Ok(mut stream)) => {
                                        sync_from_primary(
                                            &mut stream,
                                            &store,
                                            repl_password.as_deref(),
                                            &tx,
                                            &state,
                                        )
                                        .await
                                    }
                                }
                            }
                        }
                    }
                };

                if let Err(e) = result {
                    warn!("Replica: sync ended: {}", e);
                    // Sync dropped — primary may be gone; start tracking if not already.
                    unreachable_since.get_or_insert_with(std::time::Instant::now);
                }
            }
        }

        // Auto-failover: promote if primary has been unreachable long enough.
        if let (Some(timeout), Some(since)) = (failover_timeout_secs, unreachable_since) {
            let elapsed = since.elapsed().as_secs();
            if elapsed >= timeout {
                warn!(
                    "Replica: primary unreachable for {}s (timeout {}s) — auto-promoting to primary",
                    elapsed, timeout
                );
                state.promote_to_primary();
                return;
            }
            info!(
                "Replica: primary unreachable for {}s / {}s before auto-failover",
                elapsed, timeout
            );
        }

        tokio::time::sleep(tokio::time::Duration::from_secs(backoff_secs)).await;
        backoff_secs = (backoff_secs * 2).min(30);
    }
}

pub(crate) async fn sync_from_primary<S>(
    socket: &mut S,
    store: &KeyValueStore,
    repl_password: Option<&str>,
    tx: &broadcast::Sender<SyncMsg>,
    state: &ServerState,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // 0. Send auth password if configured
    if let Some(pwd) = repl_password {
        let msg = format!("{}\n", pwd);
        socket.write_all(msg.as_bytes()).await?;
        socket.flush().await?;
        // Read "+OK\n" (4 bytes)
        let mut resp = [0u8; 4];
        socket.read_exact(&mut resp).await?;
        if &resp != b"+OK\n" {
            return Err(std::io::Error::new(
                ErrorKind::PermissionDenied,
                "replication auth rejected by primary",
            ));
        }
    }

    // 1. Receive full snapshot
    let mut len_buf = [0u8; 4];
    socket.read_exact(&mut len_buf).await?;
    let snap_len = u32::from_le_bytes(len_buf) as usize;
    if snap_len > MAX_REPL_FRAME_BYTES {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            format!("snapshot frame too large ({snap_len} > {MAX_REPL_FRAME_BYTES} bytes)"),
        ));
    }
    let mut snap_bytes = vec![0u8; snap_len];
    socket.read_exact(&mut snap_bytes).await?;

    match rmp_serde::from_slice::<Vec<SnapshotEntry>>(&snap_bytes) {
        Ok(entries) => {
            let count = entries.len();
            store.restore(entries);
            info!("Replica: snapshot loaded ({} entries)", count);
        }
        Err(e) => {
            return Err(std::io::Error::new(ErrorKind::InvalidData, e.to_string()));
        }
    }

    // 2. Stream write commands from primary, acknowledging what we apply
    //
    // Every frame is counted, including one that fails to parse: the primary
    // counts frames it sent, so skipping a bad frame here would desynchronise
    // the two offsets and understate lag forever after.
    let mut applied: u64 = 0;
    loop {
        let mut len_buf = [0u8; 4];
        socket.read_exact(&mut len_buf).await?;
        let cmd_len = u32::from_le_bytes(len_buf) as usize;
        if cmd_len > MAX_REPL_FRAME_BYTES {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                format!("command frame too large ({cmd_len} > {MAX_REPL_FRAME_BYTES} bytes)"),
            ));
        }
        let mut cmd_bytes = vec![0u8; cmd_len];
        socket.read_exact(&mut cmd_bytes).await?;
        applied += 1;

        match Value::parse(&cmd_bytes) {
            Ok((value, _)) => {
                // Replication frames are broadcast as RESP3 Push (>N\r\n); normalise to
                // Array so Command::from_value can parse them.
                let normalised = match value {
                    Value::Push(inner) => Value::Array(Some(inner)),
                    other => other,
                };
                if let Ok(cmd) = Command::from_value(normalised) {
                    let keys = primary_keys(&cmd);
                    store.execute(cmd);
                    // Relay the applied write so this replica's own WebSocket
                    // clients see it, and any sub-replicas / AOF get it too
                    // (enables multi-tier replication and replica WS push).
                    let _ = tx.send(Arc::new(SyncPush {
                        origin: 0,
                        keys,
                        resp: cmd_bytes.clone(),
                    }));
                    state.on_write(&cmd_bytes).await;
                }
            }
            Err(e) => warn!("Replica: bad command from primary: {}", e),
        }

        // Acknowledge on the same socket. TcpStream is unbuffered, so this is a
        // single 8-byte write with no flush; a failure means the primary is
        // gone, which the next read will surface with a better error.
        if socket.write_all(&applied.to_le_bytes()).await.is_err() {
            warn!("Replica: failed to send replication acknowledgement");
        }
    }
}

// ── security helpers ─────────────────────────────────────────────────────────

/// Constant-time byte slice equality to prevent timing-based password leaks.
pub(crate) fn ct_eq_bytes(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

// ── sync scoping ─────────────────────────────────────────────────────────────
