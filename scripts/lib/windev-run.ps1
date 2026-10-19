# Patchbay Dev launcher for Windows, served by scripts/windev.sh. Run it with
#   irm __BASE__/run.ps1 | iex
# It downloads the latest dev build, runs it, and restarts it whenever a new build appears.
# Closing the app (Quit from the tray) or Ctrl+C here stops it.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$base = '__BASE__'
$dir = Join-Path $env:LOCALAPPDATA 'PatchbayDev'
$exe = Join-Path $dir 'patchbay-dev.exe'
$idFile = Join-Path $dir 'build-id.txt'
New-Item -ItemType Directory -Force -Path $dir | Out-Null

function Get-BuildId {
    try { (Invoke-WebRequest "$base/build-id.txt" -UseBasicParsing -TimeoutSec 5).Content.Trim() } catch { $null }
}

while ($true) {
    $id = Get-BuildId
    if (-not $id) {
        Write-Host "Waiting for a build from $base ..."
        Start-Sleep 5
        continue
    }
    Get-Process patchbay-dev -ErrorAction SilentlyContinue | Stop-Process -Force
    if (-not (Test-Path $exe) -or (Get-Content $idFile -ErrorAction SilentlyContinue) -ne $id) {
        Write-Host "Downloading build $id ..."
        Invoke-WebRequest "$base/patchbay-dev.exe" -OutFile "$exe.part" -UseBasicParsing
        Move-Item -Force "$exe.part" $exe
        Set-Content $idFile $id
    }
    Write-Host "Running build $id. UI changes reload live; new builds restart the app."
    $app = Start-Process $exe -NoNewWindow -PassThru
    $restart = $false
    while (-not $app.HasExited) {
        Start-Sleep 2
        $latest = Get-BuildId
        if ($latest -and $latest -ne $id) {
            Write-Host "New build $latest, restarting ..."
            Stop-Process -Id $app.Id -Force
            $app.WaitForExit()
            $restart = $true
        }
    }
    if (-not $restart) {
        Write-Host 'Patchbay Dev exited.'
        break
    }
}
