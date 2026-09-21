import { describe, expect, it } from 'vitest';
import { formatInstallCommandDisplay, getInstallCommandForPlatform } from './Header';

describe('Header install command', () => {
  it('uses the Windows installer for Windows browser platforms', () => {
    expect(getInstallCommandForPlatform('Windows', undefined)).toBe(
      'irm https://ctx.rs/install.ps1 | iex',
    );
    expect(getInstallCommandForPlatform('Win32', undefined)).toBe(
      'irm https://ctx.rs/install.ps1 | iex',
    );
  });

  it('uses the Unix installer for non-Windows browser platforms', () => {
    expect(getInstallCommandForPlatform('MacIntel', undefined)).toBe(
      'curl -fsSL https://ctx.rs/install | sh',
    );
    expect(getInstallCommandForPlatform('Linux x86_64', undefined)).toBe(
      'curl -fsSL https://ctx.rs/install | sh',
    );
  });

  it('preserves configured install command overrides', () => {
    expect(getInstallCommandForPlatform('Windows', 'curl -fsSL https://ade.ctx.rs/install | sh')).toBe(
      'curl -fsSL https://ade.ctx.rs/install | sh',
    );
  });

  it('formats the Windows command with a PowerShell prompt', () => {
    expect(formatInstallCommandDisplay('irm https://ctx.rs/install.ps1 | iex')).toBe(
      'PS> irm ctx.rs/install.ps1 | iex',
    );
  });
});
