export type OS = "macOS" | "Windows" | "Linux";

export const INSTALL: Record<OS, string> = {
  macOS: "brew install --cask solder",
  Windows: "winget install Solder.Solder",
  Linux: "curl -fsSL https://solder.dev/install.sh | sh",
};

export const INSTALL_PREVIEW: Record<OS, string> = {
  macOS: "brew install --cask solder@preview",
  Windows: "winget install Solder.Solder.Preview",
  Linux: "curl -fsSL https://solder.dev/install.sh | sh -s -- --preview",
};
