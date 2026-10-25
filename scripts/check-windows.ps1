# Native Windows checks. Requires Rust MSVC, C++ Build Tools and Windows SDK (fxc).
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'Run this script on Windows.' }
Set-Location (Split-Path $PSScriptRoot -Parent)

function Invoke-Cargo {
    param([string[]]$CargoArgs)
    & cargo @CargoArgs
    if ($LASTEXITCODE -ne 0) { throw "cargo $($CargoArgs -join ' ') failed" }
}

Invoke-Cargo -CargoArgs @('fmt', '--all', '--', '--check')
Invoke-Cargo -CargoArgs @('clippy', '--locked', '--workspace', '--lib', '--bins', '--', '-D', 'warnings')
Invoke-Cargo -CargoArgs @('test', '--release', '--locked', '--workspace', '--lib', '--', '--test-threads=1')
# Native UI/path/keyboard regressions. POSIX shell/server fixtures remain on macOS/Linux.
Invoke-Cargo -CargoArgs @(
    'test', '--release', '--locked', '-p', 'app', '--bin', 'codeyard', '--',
    'windows_', 'resize_', 'qa_sidebar_sash_drag', 'links::tests', 'terminal::tests', 'ui::path_tests',
    '--test-threads=1'
)
Invoke-Cargo -CargoArgs @('build', '--release', '--locked', '-p', 'app')
