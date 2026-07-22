#!/usr/bin/env python3
# ---
# relationships:
#   publishes: intent-driven-polyglot-release
# ---

import argparse
import gzip
import io
from pathlib import Path
import tarfile
import zipfile


def inputs(binary: Path, root: Path) -> list[tuple[str, bytes, int]]:
    executable_name = "intentional.exe" if binary.suffix == ".exe" else "intentional"
    return [
        (executable_name, binary.read_bytes(), 0o755),
        ("LICENSE", (root / "LICENSE").read_bytes(), 0o644),
        ("README.md", (root / "README.md").read_bytes(), 0o644),
    ]


def write_tar_gz(output: Path, artifact: str, files: list[tuple[str, bytes, int]]) -> None:
    with output.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT) as archive:
                directory = tarfile.TarInfo(f"{artifact}/")
                directory.type = tarfile.DIRTYPE
                directory.mode = 0o755
                directory.mtime = 0
                directory.uid = 0
                directory.gid = 0
                directory.uname = ""
                directory.gname = ""
                archive.addfile(directory)
                for name, contents, mode in files:
                    entry = tarfile.TarInfo(f"{artifact}/{name}")
                    entry.size = len(contents)
                    entry.mode = mode
                    entry.mtime = 0
                    entry.uid = 0
                    entry.gid = 0
                    entry.uname = ""
                    entry.gname = ""
                    archive.addfile(entry, io.BytesIO(contents))


def zip_entry(name: str, mode: int, directory: bool = False) -> zipfile.ZipInfo:
    entry = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
    entry.create_system = 3
    entry.compress_type = zipfile.ZIP_DEFLATED
    entry.external_attr = ((0o040000 if directory else 0o100000) | mode) << 16
    if directory:
        entry.external_attr |= 0x10
    return entry


def write_zip(output: Path, artifact: str, files: list[tuple[str, bytes, int]]) -> None:
    with zipfile.ZipFile(output, mode="w") as archive:
        archive.writestr(zip_entry(f"{artifact}/", 0o755, directory=True), b"")
        for name, contents, mode in files:
            archive.writestr(zip_entry(f"{artifact}/{name}", mode), contents)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifact", required=True)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--format", choices=("tar.gz", "zip"), required=True)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()

    root = Path(__file__).resolve().parents[2]
    if not args.binary.is_file():
        parser.error(f"release binary does not exist: {args.binary}")
    package_files = inputs(args.binary, root)
    if args.format == "tar.gz":
        write_tar_gz(args.output, args.artifact, package_files)
    else:
        write_zip(args.output, args.artifact, package_files)


if __name__ == "__main__":
    main()
