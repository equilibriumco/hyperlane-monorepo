import { expect } from 'chai';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';

import { OwnerStateStore } from './owner-state.js';

describe('OwnerStateStore', () => {
  let root: string;
  let stateDir: string;

  const mode = (p: string) => fs.statSync(p).mode & 0o777;

  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'owner-state-'));
    stateDir = path.join(root, 'state');
  });

  afterEach(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('writes the file readable by its owner only, in a directory only its owner can open', () => {
    new OwnerStateStore(stateDir).getOrCreate('night');
    expect(mode(path.join(stateDir, 'owner-state.json'))).to.equal(0o600);
    expect(mode(stateDir)).to.equal(0o700);
  });

  it('tightens a file a save finds readable by others', () => {
    const store = new OwnerStateStore(stateDir);
    store.getOrCreate('night');
    const file = path.join(stateDir, 'owner-state.json');
    fs.chmodSync(file, 0o644);
    store.saveMaintenanceSigningKey('aa'.repeat(32), 'key');
    expect(mode(file)).to.equal(0o600);
  });

  it('stops a deploy while the file is readable by others', () => {
    const store = new OwnerStateStore(stateDir);
    store.assertPrivate();
    store.getOrCreate('night');
    store.assertPrivate();
    fs.chmodSync(path.join(stateDir, 'owner-state.json'), 0o640);
    expect(() => store.assertPrivate()).to.throw(/readable by other users/);
  });
});
