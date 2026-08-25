/**
 * Populates ./artifacts with the compiled Compact contract modules the SDK runs
 * locally. Sources them from `HYPERLANE_MIDNIGHT_CONTRACTS` or a sibling
 * hyperlane-midnight checkout, and keeps whatever is already in ./artifacts if
 * neither is present, so a clean CI checkout still builds.
 */
import { cpSync, existsSync, mkdirSync, readdirSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const PKG_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const ARTIFACTS_DIR = resolve(PKG_ROOT, 'artifacts');
const CONTRACTS = ['night', 'igp', 'validator-announce'];

const source =
  process.env.HYPERLANE_MIDNIGHT_CONTRACTS ??
  resolve(PKG_ROOT, '../../../hyperlane-midnight/contracts/src/managed');

if (!existsSync(source)) {
  if (
    CONTRACTS.every((c) =>
      existsSync(resolve(ARTIFACTS_DIR, c, 'contract/index.js')),
    )
  ) {
    console.log(
      `midnight-sdk: no contracts source at ${source}, keeping existing artifacts`,
    );
    process.exit(0);
  }
  console.error(
    `midnight-sdk: compiled contracts not found at ${source} and ./artifacts is empty.\n` +
      `Set HYPERLANE_MIDNIGHT_CONTRACTS to a compiled contracts/src/managed tree ` +
      `(run \`npm run compile\` in the hyperlane-midnight repo first).`,
  );
  process.exit(1);
}

for (const name of CONTRACTS) {
  const contractSrc = resolve(source, name, 'contract');
  if (!existsSync(resolve(contractSrc, 'index.js'))) {
    console.error(
      `midnight-sdk: missing compiled module ${contractSrc}/index.js`,
    );
    process.exit(1);
  }
  const dest = resolve(ARTIFACTS_DIR, name);
  mkdirSync(resolve(dest, 'contract'), { recursive: true });
  for (const file of ['index.js', 'index.d.ts']) {
    cpSync(resolve(contractSrc, file), resolve(dest, 'contract', file));
  }
  // Verifier keys only exist after a full compile and are small. Prover keys
  // and zkir circuits are multi-GB, so they stay in the compiled tree and
  // proving flows point at it at runtime.
  const keysSrc = resolve(source, name, 'keys');
  if (existsSync(keysSrc)) {
    mkdirSync(resolve(dest, 'keys'), { recursive: true });
    for (const file of readdirSync(keysSrc)) {
      if (file.endsWith('.verifier')) {
        cpSync(resolve(keysSrc, file), resolve(dest, 'keys', file));
      }
    }
  }
  // midnight-js refuses ZK artifacts without the compiler's integrity manifest.
  const compilerSrc = resolve(source, name, 'compiler');
  if (existsSync(compilerSrc)) {
    cpSync(compilerSrc, resolve(dest, 'compiler'), { recursive: true });
  }
}
console.log(`midnight-sdk: artifacts copied from ${source}`);
