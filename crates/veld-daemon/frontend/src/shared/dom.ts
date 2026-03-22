const PREFIX = "veld-feedback-";

export { PREFIX };

export function mkEl(tag: string, cls?: string, text?: string): HTMLElement {
  const el = document.createElement(tag);
  if (cls) el.className = PREFIX + cls.split(" ").join(" " + PREFIX);
  if (text) el.textContent = text;
  return el;
}

export function mkBtn(
  cls: string,
  innerHTML: string,
  title?: string,
): HTMLButtonElement {
  const btn = mkEl("button", cls) as HTMLButtonElement;
  btn.innerHTML = innerHTML;
  if (title) btn.title = title;
  btn.type = "button";
  return btn;
}
