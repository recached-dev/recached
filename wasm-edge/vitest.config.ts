import path from 'node:path';
import { defineConfig } from 'vitest/config';

export default defineConfig({
  resolve: {
    alias: [
      // `sdk.js` is a committed build artifact sitting next to `sdk.ts`, so a
      // plain `./sdk.js` import in a test resolves to whatever was last built
      // — tests would silently pass against stale output. Point them at the
      // source instead; `npm run verify` is what checks the built artifact.
      { find: /^\.\/sdk\.js$/, replacement: path.resolve(import.meta.dirname, 'sdk.ts') },
    ],
  },
});
