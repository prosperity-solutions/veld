import { useEffect, useRef } from "react";

export interface MenuItem {
  label: string;
  danger?: boolean;
  disabled?: boolean;
  onClick: () => void;
}

/**
 * Minimal custom context menu — deliberately not a UI library: one menu
 * doesn't justify a dependency, and the design tokens stay ours. If menu /
 * overlay density grows, adopt headless primitives (Radix-style), never a
 * styled kit (see desktop/ARCHITECTURE.md decision log).
 */
export function ContextMenu(props: {
  x: number;
  y: number;
  items: MenuItem[];
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const close = () => props.onClose();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") props.onClose();
    };
    // Any click (incl. another right-click) or scroll outside closes.
    window.addEventListener("mousedown", close);
    window.addEventListener("blur", close);
    window.addEventListener("keydown", onKey);
    window.addEventListener("wheel", close, { passive: true });
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("blur", close);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("wheel", close);
    };
  }, [props]);

  // Clamp into the viewport (menu width/row height match the CSS).
  const width = 190;
  const height = props.items.length * 28 + 10;
  const x = Math.min(props.x, window.innerWidth - width - 8);
  const y = Math.min(props.y, window.innerHeight - height - 8);

  return (
    <div
      ref={ref}
      className="ctx-menu"
      style={{ left: x, top: y, width }}
      // Keep clicks on the menu itself from triggering the window handler
      // before the item's onClick runs.
      onMouseDown={(e) => e.stopPropagation()}
      role="menu"
    >
      {props.items.map((item) => (
        <button
          key={item.label}
          role="menuitem"
          className={`ctx-item${item.danger ? " danger" : ""}`}
          disabled={item.disabled}
          onClick={() => {
            props.onClose();
            item.onClick();
          }}
        >
          {item.label}
        </button>
      ))}
    </div>
  );
}
