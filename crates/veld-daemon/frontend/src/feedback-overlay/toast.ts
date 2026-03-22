import { refs } from "./refs";
import { mkEl } from "./helpers";
import { PREFIX } from "./constants";

export function toast(msg: string, isError?: boolean): void {
  const t = mkEl("div", "toast", msg);
  if (isError) t.style.background = "#dc2626";
  refs.shadow.appendChild(t);
  requestAnimationFrame(function () {
    t.classList.add(PREFIX + "toast-show");
  });
  setTimeout(function () {
    t.classList.remove(PREFIX + "toast-show");
    setTimeout(function () {
      t.remove();
    }, 300);
  }, 2800);
}
