import { createContext } from "react";

// undefined means no modal owner; null means the owner's portal has not mounted yet.
export const ModalOverlayContext = createContext<HTMLElement | null | undefined>(undefined);

interface ModalLayer {
  readonly root: HTMLElement;
  readonly parent: HTMLElement | null | undefined;
}

interface IsolationState {
  readonly inert: boolean;
  readonly ariaHidden: string | null;
}

const modalLayers: ModalLayer[] = [];
const isolatedBodyChildren = new Map<HTMLElement, IsolationState>();
let bodyObserver: MutationObserver | undefined;

function topModalRoot(): HTMLElement | undefined {
  for (let index = modalLayers.length - 1; index >= 0; index -= 1) {
    if (modalLayers[index].root.isConnected) return modalLayers[index].root;
  }
  return undefined;
}

function restoreIsolation(element: HTMLElement, state: IsolationState): void {
  element.inert = state.inert;
  if (state.ariaHidden === null) element.removeAttribute("aria-hidden");
  else element.setAttribute("aria-hidden", state.ariaHidden);
}

function updateModalIsolation(): void {
  const topRoot = topModalRoot();
  if (topRoot === undefined) {
    for (const [element, original] of isolatedBodyChildren) restoreIsolation(element, original);
    isolatedBodyChildren.clear();
    return;
  }
  for (const element of [...document.body.children]) {
    if (!(element instanceof HTMLElement)) continue;
    if (!isolatedBodyChildren.has(element)) {
      isolatedBodyChildren.set(element, { inert: element.inert, ariaHidden: element.getAttribute("aria-hidden") });
    }
    const original = isolatedBodyChildren.get(element);
    if (original === undefined) continue;
    if (topRoot === element) restoreIsolation(element, original);
    else {
      element.inert = true;
      element.setAttribute("aria-hidden", "true");
    }
  }
}

export function registerModalLayer(root: HTMLElement, parent: HTMLElement | null | undefined): () => void {
  const layer = { root, parent };
  const childIndex = modalLayers.findIndex((existing) => existing.parent === root);
  if (childIndex < 0) modalLayers.push(layer);
  else modalLayers.splice(childIndex, 0, layer);
  const updateLayers = () => {
    modalLayers.forEach((entry, index) => entry.root.style.setProperty("--sg-modal-depth", String(index)));
    updateModalIsolation();
  };
  updateLayers();
  if (bodyObserver === undefined) {
    bodyObserver = new MutationObserver(updateModalIsolation);
    bodyObserver.observe(document.body, { childList: true });
  }
  return () => {
    const index = modalLayers.indexOf(layer);
    if (index >= 0) modalLayers.splice(index, 1);
    root.style.removeProperty("--sg-modal-depth");
    if (modalLayers.length === 0) {
      bodyObserver?.disconnect();
      bodyObserver = undefined;
    }
    updateLayers();
  };
}

export function isActiveModalOwner(root: HTMLElement | null | undefined): boolean {
  return root !== null && topModalRoot() === root;
}

export function registerOwnedPopover(root: HTMLElement, panel: HTMLElement): () => void {
  const surface = root.querySelector<HTMLElement>(":scope > .sg-modal-surface");
  if (surface === null) return () => {};
  // The panel escapes the clipping surface, but remains part of its modal accessibility tree.
  const owned = () => surface.getAttribute("aria-owns")?.split(" ").filter(Boolean) ?? [];
  surface.setAttribute("aria-owns", [...owned(), panel.id].join(" "));
  return () => {
    const remaining = owned().filter((id) => id !== panel.id);
    if (remaining.length === 0) surface.removeAttribute("aria-owns");
    else surface.setAttribute("aria-owns", remaining.join(" "));
  };
}

export function canRestoreOverlayFocus(target: HTMLElement | null | undefined): target is HTMLElement {
  if (target === undefined || target === null || !target.isConnected
    || target.closest("[inert], [aria-hidden='true']") !== null) return false;
  const topRoot = topModalRoot();
  return topRoot === undefined || topRoot.contains(target);
}
