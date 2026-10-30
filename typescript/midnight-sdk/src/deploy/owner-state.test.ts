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
    store.saveMaintenanceSigningKeys('aa'.repeat(32), ['key']);
    expect(mode(file)).to.equal(0o600);
  });

  it('keeps the committee keys a deploy held, in committee order, or none', () => {
    const store = new OwnerStateStore(stateDir);
    const address = 'bb'.repeat(32);
    expect(store.getMaintenanceSigningKeys(address)).to.deep.equal([]);
    const keys = [
      { tag: 'schnorr', value: 'k0' },
      { tag: 'schnorr', value: 'k1' },
      { tag: 'schnorr', value: 'k2' },
    ];
    store.saveMaintenanceSigningKeys(address, keys);
    expect(store.getMaintenanceSigningKeys(address)).to.deep.equal(keys);
    store.saveMaintenanceSigningKeys(address, []);
    expect(store.getMaintenanceSigningKeys(address)).to.deep.equal([]);
  });

  it('reads an entry written before committees as that one key', () => {
    const store = new OwnerStateStore(stateDir);
    store.getOrCreate('night');
    const file = path.join(stateDir, 'owner-state.json');
    const written = JSON.parse(fs.readFileSync(file, 'utf-8'));
    written.maintenanceSigningKeys = {
      ['cc'.repeat(32)]: { tag: 'schnorr', value: 'old' },
    };
    fs.writeFileSync(file, JSON.stringify(written));
    expect(store.getMaintenanceSigningKeys('cc'.repeat(32))).to.deep.equal([
      { tag: 'schnorr', value: 'old' },
    ]);
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
