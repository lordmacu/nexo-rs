/**
 * Phase 31.5 — exception types raised by the TypeScript plugin SDK.
 *
 * The `name` property is set explicitly on each subclass so
 * `error instanceof PluginError` and `error.name === "ManifestError"`
 * keep working after TypeScript transpilation. Without that,
 * subclasses inherit `Error`'s `name === "Error"` and instanceof
 * checks become unreliable across module boundaries.
 */

export class PluginError extends Error {
  override readonly name: string = "PluginError";

  constructor(message: string) {
    super(message);
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

export class ManifestError extends PluginError {
  override readonly name: string = "ManifestError";
  /** When the failure points at a specific manifest field
   * (`plugin.id`, `plugin.version`, etc.) the SDK populates this
   * so callers can render targeted error messages.
   */
  readonly field?: string;

  constructor(message: string, field?: string) {
    super(message);
    Object.setPrototypeOf(this, ManifestError.prototype);
    this.field = field;
  }
}

export class WireError extends PluginError {
  override readonly name: string = "WireError";

  constructor(message: string) {
    super(message);
    Object.setPrototypeOf(this, WireError.prototype);
  }
}
