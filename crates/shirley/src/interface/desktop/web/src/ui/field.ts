/**
 * The product's two input treatments, defined once.
 *
 * `FIELD_SHELL` is the prominent field — the same one the chat composer uses: white surface,
 * large radius, hairline border, and a focus ring. Anything the user types into as a primary
 * control (search, composer) uses this.
 *
 * `FIELD_INSET` is the quiet fill used inside settings forms, where fields sit in a dense grid and
 * should not compete with the content around them.
 *
 * Both live here so a new input cannot silently invent a third look — the Library search box and
 * the composer must never drift apart.
 */
export const FIELD_SHELL =
  "rounded-[1.5rem] border border-line-strong bg-surface shadow-snapshot transition-[border-color,box-shadow] duration-150 ease-product focus-within:border-ink/25 focus-within:ring-2 focus-within:ring-ink/8";

export const FIELD_INSET =
  "rounded-control border border-transparent bg-inset transition-[background-color,border-color,box-shadow] duration-150 ease-product hover:bg-selected focus-within:border-line-strong focus-within:bg-surface focus-within:ring-2 focus-within:ring-ink/8";
