const values = new Map<string, string>();
const storage: Storage = {
  get length() { return values.size; },
  clear: () => values.clear(),
  getItem: (key) => values.get(key) ?? null,
  key: (index) => [...values.keys()][index] ?? null,
  removeItem: (key) => { values.delete(key); },
  setItem: (key, value) => { values.set(key, value); },
};

Object.defineProperty(globalThis, "localStorage", {
  configurable: true,
  value: storage,
});
Object.defineProperty(window, "localStorage", {
  configurable: true,
  value: storage,
});

// jsdom has no layout engine. These missing Range APIs let the real editor measure
// an empty test viewport; geometry is qualified separately in the native WebView.
if (Range.prototype.getClientRects === undefined) {
  Range.prototype.getClientRects = () => [] as unknown as DOMRectList;
}
if (Range.prototype.getBoundingClientRect === undefined) {
  Range.prototype.getBoundingClientRect = () => new DOMRect();
}
