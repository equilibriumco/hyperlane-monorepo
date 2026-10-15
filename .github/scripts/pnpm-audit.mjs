// Known vulnerabilities in what the Midnight deployment ships from this
// workspace: the CLI and everything it resolves to, including the workspace
// packages it depends on. The rest of the monorepo (infra, widgets, ...) is not
// deployed, so its advisories are listed but do not fail the run. Accepted
// advisories live in .github/audit-ignore.json, each with a reason.
//
// Needs an installed workspace (`pnpm install --prod --frozen-lockfile` is
// enough): the shipped set comes from `pnpm ls`, since the paths in
// `pnpm audit --json` stop at the first importer that reaches a package.
//
// Usage, from the repository root: node .github/scripts/pnpm-audit.mjs

import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';

const SHIPPED_ROOT = '@hyperlane-xyz/cli';
const FAILING_SEVERITIES = new Set(['high', 'critical']);

const pnpm = JSON.parse(readFileSync('package.json', 'utf8')).packageManager;
const accepted = JSON.parse(readFileSync('.github/audit-ignore.json', 'utf8'));

function run(args) {
  try {
    return execFileSync('npx', ['-y', pnpm, ...args], { encoding: 'utf8', maxBuffer: 256 * 1024 * 1024 });
  } catch (error) {
    // pnpm audit exits 1 whenever it finds anything; the report is still on stdout.
    if (!error.stdout) throw error;
    return error.stdout;
  }
}

// Every name@version the CLI and its workspace dependencies resolve to.
const shipped = new Set();
const projects = [];
function walk(deps) {
  for (const [name, dep] of Object.entries(deps ?? {})) {
    const key = `${name}@${dep.version}`;
    if (shipped.has(key)) continue;
    shipped.add(key);
    walk(dep.dependencies);
    walk(dep.optionalDependencies);
  }
}
for (const project of JSON.parse(run(['--filter', `${SHIPPED_ROOT}...`, 'ls', '--prod', '--depth', 'Infinity', '--json']))) {
  projects.push(project.name);
  walk(project.dependencies);
  walk(project.optionalDependencies);
}

const report = JSON.parse(run(['audit', '--prod', '--json']));
const rows = { fail: [], accepted: [], unshipped: [] };
for (const advisory of Object.values(report.advisories ?? {})) {
  if (!FAILING_SEVERITIES.has(advisory.severity)) continue;
  const versions = [...new Set(advisory.findings.map((f) => f.version))];
  const shippedVersions = versions.filter((v) => shipped.has(`${advisory.module_name}@${v}`));
  const label = `${advisory.github_advisory_id} ${advisory.severity} ${advisory.module_name}@${versions.join(', ')}`;
  if (!shippedVersions.length) rows.unshipped.push(label);
  else if (accepted[advisory.github_advisory_id]) {
    rows.accepted.push(`${label}\n    ${accepted[advisory.github_advisory_id]}`);
  } else rows.fail.push(`${label}\n    shipped at ${shippedVersions.join(', ')}; patched: ${advisory.patched_versions}`);
}

console.log(`${shipped.size} packages ship with ${SHIPPED_ROOT} (projects: ${projects.join(', ')})\n`);
console.log(`High or Critical advisories outside what ships (${rows.unshipped.length}):`);
for (const row of rows.unshipped) console.log(`  ${row}`);
console.log(`\nAccepted, with a reason in .github/audit-ignore.json (${rows.accepted.length}):`);
for (const row of rows.accepted) console.log(`  ${row}`);
console.log(`\nNot accepted (${rows.fail.length}):`);
for (const row of rows.fail) console.log(`  ${row}`);

if (rows.fail.length) {
  console.error('\nFAIL: fix the advisories above (an override in pnpm-workspace.yaml) or accept them with a reason.');
  process.exit(1);
}
console.log('\npnpm audit OK: no High or Critical advisory in the shipped packages is left unaccepted');
