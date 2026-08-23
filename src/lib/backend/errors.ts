import type { BooleanCapability } from "./capabilities";

export class CapabilityError extends Error {
  readonly capability: BooleanCapability;

  constructor(capability: BooleanCapability, detail?: string) {
    super(detail ?? `Backend capability "${capability}" is not available on this host`);
    this.name = "CapabilityError";
    this.capability = capability;
  }
}

export class UnimplementedError extends Error {
  readonly method: string;

  constructor(method: string) {
    super(`FrontendBackend method "${method}" is not implemented for this host yet`);
    this.name = "UnimplementedError";
    this.method = method;
  }
}

export function requireCapability(
  capabilities: { [K in BooleanCapability]: boolean },
  capability: BooleanCapability,
): void {
  if (!capabilities[capability]) {
    throw new CapabilityError(capability);
  }
}
