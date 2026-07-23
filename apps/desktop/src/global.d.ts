interface Window {
  readonly __SIGIL_THEME_PREFERENCE__?:
    | "system"
    | "sigil_light"
    | "sigil_dark"
    | "solarized_light"
    | "solarized_dark"
    | "gruvbox_dark"
    | "nord"
    | "high_contrast_dark";
}

declare module "*.svg" {
  const source: string;
  export default source;
}
