/// Model capability helpers shared by the Settings model configurator and the
/// composer's image warning.
///
/// These mirror the backend's `pricing::resolve_capabilities` (override →
/// models.dev `modalities.input` → the coarse `attachment` flag → text-only
/// defaults) so the UI never disagrees with what the chat loop resolves for a
/// turn. Keeping the resolution client-side also means a capability toggled in
/// Settings is reflected immediately, without waiting for the model list to be
/// refetched (the backend bakes the resolved `vision` bit into `list_models`).

/// Parsed subset of models.dev's `modalities` JSON, e.g.
/// `{"input":["text","image"],"output":["text"]}`. Each field is `null` when its
/// array is absent; the whole result is `null` when the JSON is missing,
/// invalid, or carries neither list.
export interface ModelModalities {
  input: string[] | null;
  output: string[] | null;
}

/// Defensive parse of a raw `modalities` JSON string. Never throws: absent /
/// empty / null / malformed input, or a value without an `input`/`output`
/// array, all collapse to `null` so callers can fall back to the coarse
/// `attachment` flag.
export function parseModalities(raw: string | null | undefined): ModelModalities | null {
  if (!raw) return null;
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!value || typeof value !== "object") return null;
  const obj = value as Record<string, unknown>;
  const list = (x: unknown): string[] | null =>
    Array.isArray(x) ? x.filter((m): m is string => typeof m === "string") : null;
  const input = list(obj.input);
  const output = list(obj.output);
  if (input === null && output === null) return null;
  return { input, output };
}

/// Resolve a model's effective input modalities, mirroring the backend:
/// user override → models.dev `modalities.input` → the coarse `attachment`
/// flag (documents + images) → text-only defaults.
export function resolveInputModalities(
  modalities: string | null | undefined,
  attachment: boolean | null | undefined,
  overrideInput: string[] | null | undefined,
): string[] {
  if (overrideInput != null) return overrideInput;
  const parsed = parseModalities(modalities);
  if (parsed?.input) return parsed.input;
  if (attachment) return ["text", "image"];
  return ["text"];
}

/// Whether a resolved input-modality list accepts images.
export function acceptsImages(input: string[]): boolean {
  return input.includes("image");
}
