const BRAND_FONT_TIMEOUT_MS = 1_500;

interface LoadBrandFontsOptions {
  readonly includeItalic?: boolean;
  readonly timeoutMs?: number;
}

function fontSource(path: string): string {
  return `url(${JSON.stringify(path)}) format("woff2")`;
}

export async function loadBrandFonts(
  options: LoadBrandFontsOptions = {},
): Promise<void> {
  if (typeof FontFace !== 'function' || !document.fonts) {
    return;
  }

  const faces = [
    new FontFace(
      'ctx Inter',
      fontSource('/fonts/inter-variable-latin.woff2'),
      { style: 'normal', weight: '100 900' },
    ),
    new FontFace(
      'ctx VT323',
      fontSource('/fonts/vt323-regular.woff2'),
      { style: 'normal', weight: '400' },
    ),
  ];

  if (options.includeItalic) {
    faces.push(
      new FontFace(
        'ctx Inter',
        fontSource('/fonts/inter-variable-italic-latin.woff2'),
        { style: 'italic', weight: '100 900' },
      ),
    );
  }

  let timeoutId: number | undefined;
  const timeout = new Promise<null>((resolve) => {
    timeoutId = window.setTimeout(
      () => resolve(null),
      options.timeoutMs ?? BRAND_FONT_TIMEOUT_MS,
    );
  });

  try {
    const loadedFaces = await Promise.race([
      Promise.all(faces.map((face) => face.load())),
      timeout,
    ]);

    if (loadedFaces) {
      for (const face of loadedFaces) {
        document.fonts.add(face);
      }
    }
  } catch {
    // Font delivery is visual enhancement; fallback fonts keep the sites usable.
  } finally {
    if (timeoutId !== undefined) {
      window.clearTimeout(timeoutId);
    }
  }
}

export function revealBrandFontGate(): void {
  document.documentElement.classList.remove('ctx-fonts-loading');
}
