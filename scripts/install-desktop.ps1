# Install the packaged desktop app for this Windows user (no admin): copies it to
# %LOCALAPPDATA%\Programs\JurisTasks, adds a Start menu shortcut, starts it. The app then
# registers itself to start with Windows in the tray ("Start with computer", on by default).
$ErrorActionPreference = 'Stop'
$src = Join-Path $PSScriptRoot '..\app\desktop\build\compose\binaries\main\app\Tasks'
$dst = Join-Path $env:LOCALAPPDATA 'Programs\JurisTasks'
if (-not (Test-Path (Join-Path $src 'Tasks.exe'))) { throw "Build it first: scripts/package-desktop.sh" }
Get-Process Tasks -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$dst*" } | Stop-Process -Force
Start-Sleep -Milliseconds 500
robocopy $src $dst /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
if ($LASTEXITCODE -ge 8) { throw "copy failed ($LASTEXITCODE)" }
$lnk = Join-Path ([Environment]::GetFolderPath('Programs')) 'Tasks.lnk'
$sh = New-Object -ComObject WScript.Shell
$s = $sh.CreateShortcut($lnk); $s.TargetPath = Join-Path $dst 'Tasks.exe'; $s.WorkingDirectory = $dst; $s.Save()
Start-Process (Join-Path $dst 'Tasks.exe')
"Installed to $dst and started."
