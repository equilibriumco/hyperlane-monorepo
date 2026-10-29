import chai, { expect } from 'chai';
import chaiAsPromised from 'chai-as-promised';
import sinon from 'sinon';

import { ProtocolType } from '@hyperlane-xyz/utils';

import { test1 } from '../consts/testChains.js';
import { ChainMetadata } from '../metadata/chainMetadataTypes.js';
import { MultiProtocolProvider } from '../providers/MultiProtocolProvider.js';
import { MultiProviderAdapter } from '../providers/MultiProviderAdapter.js';
import { Token } from '../token/Token.js';
import { MidnightHypCollateralAdapter } from '../token/adapters/MidnightTokenAdapter.js';

import { WarpCore } from './WarpCore.js';
import {
  evmMintableRecipientBytes32,
  validateMidnightRecipient,
} from './midnightRecipient.js';

// Vectors produced with Midnight's own address codec, all over the same bytes.
const FORM_HEX =
  '0x7a1e5f0c3b9d2e4f6a8b0c1d2e3f4a5b' + '6c7d8e9f0a1b2c3d4e5f6a7b8c9d0e1f';
const FORM_UNDEPLOYED =
  'mn_addr_undeployed10g097rpmn5hy765tpswju062tdk8mr5lpgdjc02wta48hryapc0snvvkpf';
const FORM_STAGENET =
  'mn_addr_stagenet10g097rpmn5hy765tpswju062tdk8mr5lpgdjc02wta48hryapc0s7c8nxa';
const FORM_ZERO =
  'mn_addr_undeployed1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqze2h07';

chai.use(chaiAsPromised);

const EVM_ADDRESS = '0x1111111111111111111111111111111111111111';
const EVM_PADDED = '0x' + '00'.repeat(12) + '11'.repeat(20);

const midnight: ChainMetadata = {
  name: 'midnighttest',
  protocol: ProtocolType.Midnight,
  chainId: 7777,
  domainId: 7777,
  rpcUrls: [{ http: 'http://localhost:9944' }],
  midnightNetworkId: 'undeployed',
};

describe('validateMidnightRecipient', () => {
  it('Takes the unshielded address form of the destination network', () => {
    expect(validateMidnightRecipient(FORM_UNDEPLOYED, midnight)).to.be.null;
  });

  it('Refuses hex', () => {
    expect(validateMidnightRecipient(FORM_HEX, midnight)).to.deep.equal({
      recipient:
        'Midnight recipients are given in the unshielded address form (mn_addr...), not as hex',
    });
  });

  it('Refuses another network, a damaged form and the zero address', () => {
    expect(validateMidnightRecipient(FORM_STAGENET, midnight)).to.deep.equal({
      recipient:
        'Recipient address is for the stagenet network, midnighttest is undeployed',
    });
    const damaged = validateMidnightRecipient(
      FORM_UNDEPLOYED.slice(0, -1) + 'g',
      midnight,
    );
    expect(damaged?.recipient).to.match(/^Invalid Midnight address: /);
    expect(validateMidnightRecipient(FORM_ZERO, midnight)).to.deep.equal({
      recipient: 'Invalid recipient',
    });
  });

  it('Skips the network check when the chain names no network', () => {
    const { midnightNetworkId: _unused, ...noNetwork } = midnight;
    expect(validateMidnightRecipient(FORM_STAGENET, noNetwork)).to.be.null;
  });

  it('Is what WarpCore applies to a Midnight destination', () => {
    const multiProvider = new MultiProtocolProvider({
      [test1.name]: test1,
      [midnight.name]: midnight,
    });
    const warpCore = new WarpCore(multiProvider, [
      Token.FromChainMetadataNativeToken(test1),
    ]);
    expect(warpCore.validateRecipient(FORM_UNDEPLOYED, midnight.name)).to.be
      .null;
    expect(
      warpCore.validateRecipient(FORM_HEX, midnight.name)?.recipient,
    ).to.match(/unshielded address form/);
    // Other destinations keep the upstream check.
    expect(warpCore.validateRecipient(EVM_ADDRESS, test1.name)).to.be.null;
    expect(warpCore.validateRecipient(EVM_PADDED, test1.name)).to.deep.equal({
      recipient: 'Invalid recipient',
    });
  });
});

describe('evmMintableRecipientBytes32', () => {
  it('Takes a 20-byte address or its zero-padded form', () => {
    expect(evmMintableRecipientBytes32(EVM_ADDRESS)).to.equal(EVM_PADDED);
    expect(evmMintableRecipientBytes32(EVM_PADDED)).to.equal(EVM_PADDED);
  });

  it('Refuses the zero address and non-zero high bytes', () => {
    expect(() => evmMintableRecipientBytes32('0x' + '00'.repeat(32))).to.throw(
      /zero address/,
    );
    expect(() => evmMintableRecipientBytes32(FORM_HEX)).to.throw(
      /not an EVM address/,
    );
    expect(() => evmMintableRecipientBytes32(FORM_UNDEPLOYED)).to.throw(
      /not an EVM address/,
    );
  });
});

describe('MidnightHypCollateralAdapter.populateTransferRemoteTx', () => {
  const TOKEN = 'ab'.repeat(32);
  let sandbox: sinon.SinonSandbox;
  let adapter: MidnightHypCollateralAdapter;
  let built: Array<{ recipient: string }>;

  beforeEach(() => {
    sandbox = sinon.createSandbox();
    built = [];
    sandbox
      .stub(MultiProviderAdapter.prototype, 'getMidnightProvider')
      .returns({
        getRemoteTransferTransaction: async (req: { recipient: string }) => {
          built.push(req);
          return req;
        },
      } as any);
    adapter = new MidnightHypCollateralAdapter(
      midnight.name,
      new MultiProviderAdapter({
        [test1.name]: test1,
        [midnight.name]: midnight,
      }),
      { token: TOKEN },
    );
  });

  afterEach(() => {
    sandbox.restore();
  });

  function transfer(destination: number, recipient: string) {
    return adapter.populateTransferRemoteTx({
      weiAmountOrId: 1n,
      destination,
      recipient,
      fromAccountOwner: 'sender',
      customHook: 'cd'.repeat(32),
      interchainGas: { igpQuote: { addressOrDenom: 'night', amount: 1n } },
    });
  }

  it('Pads an EVM recipient and refuses one the destination cannot mint to', async () => {
    await transfer(test1.domainId!, EVM_ADDRESS);
    expect(built[0].recipient).to.equal(EVM_PADDED.slice(2));
    await expect(
      transfer(test1.domainId!, '0x' + '00'.repeat(32)),
    ).to.be.rejectedWith(/zero address/);
    await expect(transfer(test1.domainId!, FORM_HEX)).to.be.rejectedWith(
      /not an EVM address/,
    );
  });

  it('Leaves a recipient for a destination it does not know alone', async () => {
    await transfer(424242, FORM_HEX);
    expect(built[0].recipient).to.equal(FORM_HEX.slice(2));
  });
});
