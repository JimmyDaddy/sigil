export async function writeClipboard(text: string): Promise<boolean> {
  if (navigator.clipboard === undefined) return false;
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}
