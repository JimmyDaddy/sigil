import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, extname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const desktopRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const srcRoot = join(desktopRoot, "src");
const foundations = join(srcRoot, "ui", "foundations");
const stylesPath = join(srcRoot, "styles.css");
const referencePath = join(foundations, "reference.css");
const themesPath = join(foundations, "themes.css");
const packagePath = join(desktopRoot, "package.json");
const indexPath = join(desktopRoot, "index.html");
const appearanceBootstrapPath = join(desktopRoot, "public", "appearance-bootstrap.js");

function fail(message) {
  throw new Error(`desktop UI system check failed: ${message}`);
}

function declarations(source) {
  return new Map(
    [...source.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)].map((match) => [match[1], match[2].trim()]),
  );
}

function markerBlock(source, name) {
  const start = `/* @theme ${name}-start */`;
  const end = `/* @theme ${name}-end */`;
  const startIndex = source.indexOf(start);
  const endIndex = source.indexOf(end);
  if (startIndex < 0 || endIndex <= startIndex) {
    fail(`missing ${name} theme marker block`);
  }
  return source.slice(startIndex + start.length, endIndex);
}

function resolveVariable(name, theme, refs, seen = new Set()) {
  if (seen.has(name)) fail(`cyclic token reference at ${name}`);
  seen.add(name);
  const value = theme.get(name) ?? refs.get(name);
  if (value === undefined) fail(`unresolved token ${name}`);
  const variable = value.match(/^var\((--[\w-]+)\)$/);
  return variable === null ? value : resolveVariable(variable[1], theme, refs, seen);
}

function rgb(hex) {
  const match = hex.match(/^#([0-9a-f]{6})$/i);
  if (match === null) fail(`contrast token is not an opaque six-digit hex color: ${hex}`);
  const value = Number.parseInt(match[1], 16);
  return [(value >> 16) & 255, (value >> 8) & 255, value & 255];
}

function luminance(hex) {
  return rgb(hex)
    .map((channel) => channel / 255)
    .map((channel) => (channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4))
    .reduce((sum, channel, index) => sum + channel * [0.2126, 0.7152, 0.0722][index], 0);
}

function contrast(foreground, background) {
  const [lighter, darker] = [luminance(foreground), luminance(background)].sort((a, b) => b - a);
  return (lighter + 0.05) / (darker + 0.05);
}

function walk(directory) {
  return readdirSync(directory).flatMap((entry) => {
    const path = join(directory, entry);
    return statSync(path).isDirectory() ? walk(path) : [path];
  });
}

function resolveImport(from, specifier) {
  if (!specifier.startsWith(".")) return null;
  const base = resolve(dirname(from), specifier);
  const candidates = [base, ...[".ts", ".tsx", ".js", ".jsx", ".css"].map((suffix) => `${base}${suffix}`),
    ...[".ts", ".tsx", ".js", ".jsx"].map((suffix) => join(base, `index${suffix}`))];
  return candidates.find((candidate) => {
    try { return statSync(candidate).isFile(); } catch { return false; }
  }) ?? null;
}

function productionGraph(entrypoint) {
  const visited = new Set();
  const visit = (path) => {
    if (visited.has(path) || extname(path) === ".css") return;
    visited.add(path);
    const source = readFileSync(path, "utf8");
    const imports = [
      ...source.matchAll(/(?:import|export)\s+(?:[^"']*?\s+from\s+)?["']([^"']+)["']/g),
    ];
    for (const match of imports) {
      const target = resolveImport(path, match[1]);
      if (target !== null) visit(target);
    }
  };
  visit(entrypoint);
  return visited;
}

const styles = readFileSync(stylesPath, "utf8");
const expectedLayers = "@layer reset, tokens, base, primitives, patterns, features, utilities;";
if (!styles.includes(expectedLayers)) fail("CSS layer order is not frozen");

for (const file of ["reference.css", "themes.css", "density.css", "elevation.css", "motion.css", "reset.css", "typography.css", "forced-colors.css"]) {
  if (!styles.includes(`./ui/foundations/${file}`)) fail(`styles.css does not import ${file}`);
}

const allowedRawColorFiles = new Set([referencePath, join(foundations, "forced-colors.css")]);
for (const path of walk(srcRoot).filter((candidate) => extname(candidate) === ".css")) {
  if (allowedRawColorFiles.has(path)) continue;
  const source = readFileSync(path, "utf8");
  if (/(?:#[0-9a-f]{3,8}\b|\brgba?\s*\()/i.test(source)) {
    fail(`raw color outside reference/forced-color foundations: ${relative(desktopRoot, path)}`);
  }
}

const references = declarations(readFileSync(referencePath, "utf8"));
const themeSource = readFileSync(themesPath, "utf8");
const themes = {
  dark: declarations(markerBlock(themeSource, "dark")),
  light: declarations(markerBlock(themeSource, "light")),
};
const rolePrefix = /^(--sg-sys-color-|--sg-domain-color-|--sg-sys-shadow-)/;
const darkRoles = [...themes.dark.keys()].filter((name) => rolePrefix.test(name)).sort();
const lightRoles = [...themes.light.keys()].filter((name) => rolePrefix.test(name)).sort();
if (JSON.stringify(darkRoles) !== JSON.stringify(lightRoles)) fail("light/dark semantic role parity differs");

const contrastPairs = [
  ["--sg-sys-color-on-surface", "--sg-sys-color-surface"],
  ["--sg-sys-color-on-primary", "--sg-sys-color-primary"],
  ["--sg-sys-color-on-primary-container", "--sg-sys-color-primary-container"],
  ["--sg-sys-color-on-error", "--sg-sys-color-error"],
  ["--sg-sys-color-on-error-container", "--sg-sys-color-error-container"],
  ["--sg-domain-color-on-success", "--sg-domain-color-success"],
  ["--sg-domain-color-on-warning", "--sg-domain-color-warning"],
  ["--sg-domain-color-on-danger", "--sg-domain-color-danger"],
  ["--sg-domain-color-on-info", "--sg-domain-color-info"],
];
for (const [themeName, theme] of Object.entries(themes)) {
  for (const [foregroundRole, backgroundRole] of contrastPairs) {
    const foreground = resolveVariable(foregroundRole, theme, references);
    const background = resolveVariable(backgroundRole, theme, references);
    const ratio = contrast(foreground, background);
    if (ratio < 4.5) fail(`${themeName} ${foregroundRole}/${backgroundRole} contrast is ${ratio.toFixed(2)}:1`);
  }
}

const packageJson = JSON.parse(readFileSync(packagePath, "utf8"));
const dependencies = { ...packageJson.dependencies, ...packageJson.devDependencies };
for (const dependency of Object.keys(dependencies)) {
  if (dependency === "@base-ui/react" || dependency.startsWith("@mui/") || dependency === "@material/web") {
    fail(`unapproved UI runtime dependency ${dependency}`);
  }
}

const graph = productionGraph(join(srcRoot, "main.tsx"));
if ([...graph].some((path) => path.includes(`${join("ui", "catalog")}`))) {
  fail("development UI catalog is reachable from the production entrypoint");
}
if (!readFileSync(join(srcRoot, "ui", "catalog", "fixtures.ts"), "utf8").includes("sigil-desktop-dev-ui-catalog")) {
  fail("development UI catalog marker is missing");
}

const indexSource = readFileSync(indexPath, "utf8");
if (!indexSource.includes('<script src="/appearance-bootstrap.js"></script>')) {
  fail("external pre-paint appearance bootstrap is missing");
}
if (/<script(?![^>]*\bsrc=)[^>]*>[\s\S]*?<\/script>/i.test(indexSource)) {
  fail("inline desktop bootstrap scripts are forbidden");
}
const appearanceBootstrap = readFileSync(appearanceBootstrapPath, "utf8");
for (const forbidden of ["localStorage", "sessionStorage", "fetch(", "invoke(", "token", "bearer"]) {
  if (appearanceBootstrap.includes(forbidden)) fail(`appearance bootstrap contains forbidden capability: ${forbidden}`);
}
if (!themeSource.includes(':root[data-theme="light"]')) {
  fail("light theme must be selected by the pre-paint data-theme contract");
}

console.log(`desktop UI system checks passed (${darkRoles.length} paired theme roles, ${contrastPairs.length * 2} contrast checks)`);
