const MAX_CACHE_ENTRIES = 64;

export interface DiagramCacheEntry {
  readonly svg: string;
  readonly diagramType: string;
}

const diagramCache = new Map<string, DiagramCacheEntry>();

export function diagramCacheKey(source: string, theme: string): string {
  return `${theme}:${stableHash(source)}`;
}

export function readDiagramCache(key: string): DiagramCacheEntry | undefined {
  const entry = diagramCache.get(key);
  if (entry === undefined) return undefined;
  diagramCache.delete(key);
  diagramCache.set(key, entry);
  return entry;
}

export function writeDiagramCache(key: string, entry: DiagramCacheEntry): void {
  diagramCache.delete(key);
  diagramCache.set(key, entry);
  while (diagramCache.size > MAX_CACHE_ENTRIES) {
    const oldest = diagramCache.keys().next().value as string | undefined;
    if (oldest === undefined) break;
    diagramCache.delete(oldest);
  }
}

export function clearDiagramCache(): void {
  diagramCache.clear();
}

function stableHash(source: string): string {
  let hash = 0x811c9dc5;
  for (const character of source) {
    hash ^= character.codePointAt(0) ?? 0;
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16).padStart(8, "0");
}
