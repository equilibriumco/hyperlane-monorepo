import {
  assertIgpOracles,
  assertOverheadDomainsHaveOracles,
} from '../hook/hook-artifact-manager.js';
import { assertOwnerKept } from '../utils/errors.js';

type OracleRates = { gasPrice: string; tokenExchangeRate: string };

interface IgpBlock {
  type: 'interchainGasPaymaster';
  owner?: string;
  overhead?: Record<string, number>;
  oracleConfig?: Record<string, OracleRates>;
}

function isIgpBlock(hook: unknown): hook is IgpBlock {
  return (
    typeof hook === 'object' &&
    hook !== null &&
    (hook as { type?: unknown }).type === 'interchainGasPaymaster'
  );
}

// A core deploy sends night before it reaches the owner and the IGP, so a
// value either refuses would leave night deployed and the rest not. Checked
// before anything is sent.
export function assertCoreDeployConfig(config: {
  owner?: string;
  defaultHook?: unknown;
  requiredHook?: unknown;
}): void {
  assertOwnerKept('night', config.owner);
  for (const hook of [config.defaultHook, config.requiredHook]) {
    if (!isIgpBlock(hook)) continue;
    assertOwnerKept('the IGP', hook.owner);
    assertOverheadDomainsHaveOracles(hook);
    assertIgpOracles(hook);
  }
}
