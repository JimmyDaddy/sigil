interface Window {
  readonly __SIGIL_THEME_PREFERENCE__?: "system" | "light" | "dark";
}

declare module "*.svg" {
  const source: string;
  export default source;
}
