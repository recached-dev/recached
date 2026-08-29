//! Two "pods" sharing one cache.
//!
//! The point of the crate in 40 lines: pod B never asks the server for the
//! fare, yet sees pod A's write. Reads are local memory; the socket only
//! carried the change notice.
//!
//! Run a server first:  `cargo run -p server-native`
//! Then:                `cargo run -p recached-embed --example two_pods`

use recached_embed::{Cache, Error};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("RECACHED_URL").unwrap_or_else(|_| "ws://127.0.0.1:6380".to_string());

    let pod_a = Cache::connect(&url).await?;
    let pod_b = Cache::connect(&url).await?;
    println!("both pods connected to {url}");

    // Each pod declares its working set once, at start-up.
    pod_a.watch("fare:*").await?;
    pod_b.watch("fare:*").await?;
    println!("both pods hydrated `fare:*`");

    // Pod A writes.
    pod_a.set("fare:MNL-CEB", "1850").await?;
    println!("pod A wrote fare:MNL-CEB = 1850");

    // The change is pushed to every watcher. Give it a moment to land.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Pod B reads from its OWN memory — no round-trip.
    let fare = pod_b.get_str("fare:MNL-CEB")?;
    println!("pod B read  fare:MNL-CEB = {fare:?}   <- from local memory");
    assert_eq!(fare.as_deref(), Some("1850"));

    // An update propagates the same way.
    pod_a.set("fare:MNL-CEB", "1925").await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    println!(
        "after update, pod B sees {:?}",
        pod_b.get_str("fare:MNL-CEB")?
    );
    assert_eq!(pod_b.get_str("fare:MNL-CEB")?.as_deref(), Some("1925"));

    // A deletion, too.
    pod_a.del("fare:MNL-CEB").await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    println!(
        "after delete, pod B sees {:?}",
        pod_b.get_str("fare:MNL-CEB")?
    );
    assert_eq!(pod_b.get_str("fare:MNL-CEB")?, None);

    // The safety net: a key nobody watched is an error, not a silent `None`.
    match pod_b.get("user:42") {
        Err(Error::NotHydrated { .. }) => {
            println!("\nunwatched key -> NotHydrated (not a silent None)");
        }
        other => panic!("expected NotHydrated, got {other:?}"),
    }

    // ...and the escape hatch pays for a round-trip instead.
    pod_a
        .set_ex("user:42", "dencio", Duration::from_secs(60))
        .await?;
    let fetched = pod_b.get_or_fetch("user:42").await?;
    println!(
        "get_or_fetch(\"user:42\") -> {:?}   <- one round-trip",
        fetched.map(|b| String::from_utf8_lossy(&b).into_owned())
    );

    println!(
        "\npod B: connected={} pending_writes={} local_bytes={}",
        pod_b.is_connected(),
        pod_b.pending_writes(),
        pod_b.local_bytes(),
    );
    Ok(())
}
