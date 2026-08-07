// The veld icon mark (`V` + accent-green dot) — the same geometry as
// `logo.svg` at the repo root, which docs/branding.md makes canonical. The
// source file hardcodes white/#C4F56A because it is also rasterised for app
// icons; here the paths take the theme tokens instead, so the mark is legible
// in both palettes. This is `/ide`'s brand surface: the top bar shows it
// beside the current mode.
export function LogoMark() {
  return (
    <svg
      className="logo-mark"
      viewBox="0 0 32 32"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      aria-hidden="true"
    >
      <path d="M13.2 28L4 4H8.4L15.7 23.8H15.8L23.1 4H27.5L18.3 28H13.2Z" />
      <path d="M24.5 29C25.8807 29 27 27.8807 27 26.5C27 25.1193 25.8807 24 24.5 24C23.1193 24 22 25.1193 22 26.5C22 27.8807 23.1193 29 24.5 29Z" />
    </svg>
  );
}
