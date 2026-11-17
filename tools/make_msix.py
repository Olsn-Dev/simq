#!/usr/bin/env python3
"""Lays out the folder for the MSIX package (Microsoft Store) of SimQ.

Writes AppxManifest.xml, the logo pictures in the sizes MSIX asks for, and
copies the program. The release workflow then packs the folder with
`makeappx pack`. Production packages are not signed here: they are uploaded
to Microsoft Partner Center, which signs them.

The version comes from Cargo.toml, the single source of truth, and is turned
into the four-part form MSIX needs (Major.Minor.Build.0).

Three values come from Partner Center (Product identity) and are given as
options or environment variables; the defaults only make a package that can
be inspected, not one the Store accepts:

  --identity-name         MSIX_IDENTITY_NAME            e.g. 12345Team.SimQ
  --publisher             MSIX_PUBLISHER                e.g. CN=1A2B3C4D-...
  --publisher-display     MSIX_PUBLISHER_DISPLAY_NAME   e.g. Team Racing

Requires Pillow:  python3 tools/make_msix.py --exe target/release/simq.exe --out dist/msix
"""
import argparse
import os
import pathlib
import re
import shutil
import sys
from xml.sax.saxutils import escape, quoteattr

from PIL import Image

ROOT = pathlib.Path(__file__).resolve().parent.parent
ICON = ROOT / "assets" / "icon.png"

# Square logos: file name -> size in pixels.
SQUARE = {
    "StoreLogo.png": 50,
    "Square44x44Logo.png": 44,
    "SmallTile.png": 71,
    "Square150x150Logo.png": 150,
    "LargeTile.png": 310,
}
# Crisp taskbar and Start icons without the coloured plate behind them.
TARGET_SIZES = (16, 24, 32, 48, 256)


def cargo_version():
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    package = text.split("[package]", 1)[1].split("\n[", 1)[0]
    match = re.search(r'^version\s*=\s*"([^"]+)"', package, re.MULTILINE)
    if not match:
        sys.exit("no version in Cargo.toml")
    return match.group(1)


def msix_version(version):
    """`1.2.3` (or `1.2.3-beta`) -> `1.2.3.0`. MSIX wants four numbers, each
    at most 65535, and the Store wants the last one to be 0."""
    numbers = re.match(r"^(\d+)\.(\d+)\.(\d+)", version)
    if not numbers:
        raise ValueError(f"not a version: {version}")
    parts = [int(n) for n in numbers.groups()]
    if any(n > 65535 for n in parts):
        raise ValueError(f"a part of the version is larger than 65535: {version}")
    return ".".join(str(n) for n in parts) + ".0"


def square(icon, size):
    return icon.resize((size, size), Image.LANCZOS)


def wide(icon, width, height):
    """The icon centred on a transparent wide tile."""
    tile = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    side = int(height * 0.8)
    logo = square(icon, side)
    tile.paste(logo, ((width - side) // 2, (height - side) // 2), logo)
    return tile


def manifest(version, identity, publisher, publisher_display):
    return f"""<?xml version="1.0" encoding="utf-8"?>
<!-- Written by tools/make_msix.py. Do not edit by hand. -->
<Package
  xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"
  xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10"
  xmlns:rescap="http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities"
  xmlns:desktop6="http://schemas.microsoft.com/appx/manifest/desktop/windows10/6"
  IgnorableNamespaces="uap rescap desktop6">
  <Identity Name={quoteattr(identity)} Publisher={quoteattr(publisher)}
            Version="{version}" ProcessorArchitecture="x64" />
  <Properties>
    <DisplayName>SimQ</DisplayName>
    <PublisherDisplayName>{escape(publisher_display)}</PublisherDisplayName>
    <Logo>Assets\\StoreLogo.png</Logo>
    <!-- Settings, cache and the GitHub mirror are written to the real
         %APPDATA% and %LOCALAPPDATA%, not to a private copy for the package,
         so the Store version and the portable program share them. -->
    <desktop6:FileSystemWriteVirtualization>disabled</desktop6:FileSystemWriteVirtualization>
  </Properties>
  <Dependencies>
    <TargetDeviceFamily Name="Windows.Desktop" MinVersion="10.0.19041.0" MaxVersionTested="10.0.26100.0" />
  </Dependencies>
  <Resources>
    <Resource Language="en-us" />
    <Resource Language="sv-se" />
  </Resources>
  <Applications>
    <Application Id="SimQ" Executable="SimQ.exe" EntryPoint="Windows.FullTrustApplication">
      <uap:VisualElements
        DisplayName="SimQ"
        Description="Queue for simulation requests in a Formula Student team"
        BackgroundColor="transparent"
        Square150x150Logo="Assets\\Square150x150Logo.png"
        Square44x44Logo="Assets\\Square44x44Logo.png">
        <uap:DefaultTile
          Wide310x150Logo="Assets\\Wide310x150Logo.png"
          Square71x71Logo="Assets\\SmallTile.png"
          Square310x310Logo="Assets\\LargeTile.png" />
      </uap:VisualElements>
    </Application>
  </Applications>
  <Capabilities>
    <Capability Name="internetClient" />
    <rescap:Capability Name="runFullTrust" />
    <!-- Needed for FileSystemWriteVirtualization above. Partner Center asks
         why: "settings are shared with the portable version of the app". -->
    <rescap:Capability Name="unvirtualizedResources" />
  </Capabilities>
</Package>
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--exe", required=True, help="the built simq.exe")
    parser.add_argument("--out", required=True, help="folder to lay the package out in")
    parser.add_argument("--version", default=None, help="defaults to the version in Cargo.toml")
    parser.add_argument("--identity-name", default=os.environ.get("MSIX_IDENTITY_NAME") or "SimQ.SimQ")
    parser.add_argument("--publisher", default=os.environ.get("MSIX_PUBLISHER") or "CN=SimQ")
    parser.add_argument(
        "--publisher-display",
        default=os.environ.get("MSIX_PUBLISHER_DISPLAY_NAME") or "SimQ",
    )
    args = parser.parse_args()

    version = msix_version(args.version or cargo_version())
    out = pathlib.Path(args.out)
    if out.exists():
        shutil.rmtree(out)
    assets = out / "Assets"
    assets.mkdir(parents=True)

    icon = Image.open(ICON).convert("RGBA")
    for name, size in SQUARE.items():
        square(icon, size).save(assets / name, optimize=True)
    for size in TARGET_SIZES:
        name = f"Square44x44Logo.targetsize-{size}_altform-unplated.png"
        square(icon, size).save(assets / name, optimize=True)
    wide(icon, 310, 150).save(assets / "Wide310x150Logo.png", optimize=True)

    shutil.copyfile(args.exe, out / "SimQ.exe")
    (out / "AppxManifest.xml").write_text(
        manifest(version, args.identity_name, args.publisher, args.publisher_display),
        encoding="utf-8",
    )
    print(f"laid out MSIX {version} in {out}")


if __name__ == "__main__":
    main()
