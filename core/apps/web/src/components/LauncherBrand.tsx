import type { ReactNode } from "react";

type LauncherBrandProps = {
  fullScreen?: boolean;
  className?: string;
  children?: ReactNode;
};

export default function LauncherBrand({ fullScreen = false, className, children }: LauncherBrandProps) {
  const classes = ["launcher-brand", fullScreen ? "launcher-brand--full" : null, className]
    .filter(Boolean)
    .join(" ");

  return (
    <div className={classes} aria-label="ctx">
      <svg className="launcher-crt-svg" aria-hidden="true">
        <defs>
          <filter id="noise">
            <feTurbulence
              type="fractalNoise"
              baseFrequency="0.80"
              numOctaves="2"
              result="turbulence"
            />
            <feColorMatrix
              type="matrix"
              values="0 0 0 0 0
                      0 0 0 0 0
                      0 0 0 0 0
                      0 0 0 20 -10"
              result="noiseAlpha"
            />
          </filter>
        </defs>
      </svg>
      <div className="bezel-container">
        <div className="crt-screen">
          <div className="launcher-brand-stack">
            <div className="terminal-content">
              <span id="typed-text">ctx</span>
              <span className="cursor cursor--block" aria-hidden="true" />
            </div>
            {children}
          </div>
        </div>
      </div>
    </div>
  );
}
