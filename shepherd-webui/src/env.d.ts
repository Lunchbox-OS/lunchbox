/**
 * Ambient declarations for non-code imports.
 *
 * `?raw` gives a file's contents as a string at build time. Both toolchains
 * that compile this tree implement it — rspack for the bundles, Vite for the
 * tests — but neither teaches `tsc` about it, so it is declared here.
 */
declare module "*?raw" {
  const content: string;
  export default content;
}
