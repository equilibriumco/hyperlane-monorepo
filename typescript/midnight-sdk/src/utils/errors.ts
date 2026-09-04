export function unsupportedOnMidnight(kind: string, type: string): () => never {
  return () => {
    throw new Error(`unsupported ${kind} type on Midnight: ${type}`);
  };
}
