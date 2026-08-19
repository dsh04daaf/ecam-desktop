<#
  ecam-wrapper.ps1 - prueba de concepto del runtime del wrapper de Apple Music en Windows.
  Es la referencia de lo que despues hara la app: importar la distro, arrancar el wrapper,
  loguear con pantalla (usuario/clave + 2FA por archivo) y no re-loguear si ya hay sesion.

  Uso:
    .\ecam-wrapper.ps1 install  -Tar .\ecam-rootfs-slim.tar.gz
    .\ecam-wrapper.ps1 import-session -Session .\ecam-session.tar.gz
    .\ecam-wrapper.ps1 start
    .\ecam-wrapper.ps1 login
    .\ecam-wrapper.ps1 status
    .\ecam-wrapper.ps1 logout
    .\ecam-wrapper.ps1 uninstall
#>
param(
  [Parameter(Position=0)][ValidateSet('install','import-session','start','login','status','logout','uninstall')]
  [string]$Action = 'status',
  [string]$Tar,
  [string]$Session,
  [string]$Distro = 'ECAM'
)

$ErrorActionPreference = 'Stop'
$DistroDir  = Join-Path $env:LOCALAPPDATA 'ECAM\distro'
$TokenDb    = '/app/rootfs/data/data/com.apple.android.music/files/mpl_db/kvs.sqlitedb'
$TwoFaFile  = '/app/rootfs/data/data/com.apple.android.music/files/2fa.txt'
$Ports      = @(10020, 20020, 30020)   # decrypt / m3u8 / account

function Say($m, $c='Gray') { Write-Host $m -ForegroundColor $c }

function Test-Wsl {
  $wsl = Get-Command wsl.exe -ErrorAction SilentlyContinue
  if (-not $wsl) {
    Say "WSL no esta instalado." Red
    Say "Ejecuta como administrador:  wsl --install --no-distribution" Yellow
    Say "(pide reinicio; necesita virtualizacion activada en la BIOS)" Yellow
    return $false
  }
  return $true
}

function Test-Distro {
  # -l -v sale en UTF-16; normalizamos
  $list = (wsl.exe -l -q) -replace "`0", ''
  return ($list -split "`r?`n" | ForEach-Object { $_.Trim() }) -contains $Distro
}

# Corre un comando dentro de la distro como root y devuelve la salida
function Wsl-Run([string]$cmd) {
  return (wsl.exe -d $Distro -u root -- /bin/sh -c $cmd) 2>&1
}

function Has-Session {
  wsl.exe -d $Distro -u root -- /bin/sh -c "[ -f $TokenDb ]" | Out-Null
  return ($LASTEXITCODE -eq 0)
}

function Wrapper-Running {
  $r = Wsl-Run "ps | grep -c '[/]system/bin/main'"
  return ([int]($r | Select-Object -First 1) -gt 0)
}

# --- Lanza el wrapper y va reportando los estados que imprime en stderr ---
function Start-Wrapper([string]$User, [string]$Pass) {
  $inner = if ($User) {
      "cd /app && exec ./wrapper -L '$User`:$Pass' -F -H 0.0.0.0"
    } else {
      "cd /app && exec ./wrapper -H 0.0.0.0"
    }

  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName  = 'wsl.exe'
  $psi.Arguments = "-d $Distro -u root -- /bin/sh -c `"$inner`""
  $psi.RedirectStandardError  = $true
  $psi.RedirectStandardOutput = $true
  $psi.UseShellExecute = $false
  $psi.CreateNoWindow  = $true

  $p = [System.Diagnostics.Process]::Start($psi)
  $listening = 0
  $needCode  = $false

  while (-not $p.HasExited -or -not $p.StandardError.EndOfStream) {
    $line = $p.StandardError.ReadLine()
    if ($null -eq $line) { break }

    switch -Regex ($line) {
      '2FA: true'                  { $needCode = $true; Say "-> Apple pide codigo de verificacion" Cyan }
      'Waiting for input'          {
          # ventana de 60s exactos: 20 sondeos de 3s y el wrapper se sale solo
          $code = Read-Host "   Codigo 2FA (6 digitos)"
          Wsl-Run "printf '%s' '$code' > $TwoFaFile" | Out-Null
      }
      'Code file detected'         { Say "-> codigo aceptado, entrando..." Cyan }
      'Failed to get 2FA Code'     { Say "El codigo vencio (60s). Hay que reintentar el login." Red }
      'server message: (.+)'       { Say ("Apple dice: " + $Matches[1]) Yellow }
      'auth error: code=(\d+)'     { Say ("Error de autenticacion (codigo " + $Matches[1] + ")") Red }
      'login failed'               { Say "Login fallido." Red }
      'listening .*:(\d+)'         {
          $listening++
          if ($listening -ge 3) { Say "Wrapper listo y escuchando en 10020 / 20020 / 30020." Green }
      }
      default { if ($env:ECAM_DEBUG) { Say "   | $line" DarkGray } }
    }
  }
  return $p
}

switch ($Action) {

  'install' {
    if (-not (Test-Wsl)) { break }
    if (Test-Distro) { Say "La distro '$Distro' ya existe. Nada que hacer." Green; break }
    if (-not $Tar -or -not (Test-Path $Tar)) { Say "Falta -Tar con la ruta del rootfs .tar.gz" Red; break }
    New-Item -ItemType Directory -Force -Path $DistroDir | Out-Null
    Say "Importando la distro (esto tarda un poco la primera vez)..."
    wsl.exe --import $Distro $DistroDir $Tar --version 2
    if ($LASTEXITCODE -ne 0) { Say "Fallo la importacion." Red; break }
    Say "Distro '$Distro' instalada en $DistroDir" Green
  }

  'import-session' {
    # Mete una sesion ya iniciada (sacada del VPS) sin tener que loguear.
    if (-not (Test-Distro)) { Say "No esta instalada la distro. Corre: install" Red; break }
    if (-not $Session -or -not (Test-Path $Session)) { Say "Falta -Session con la ruta del .tar.gz" Red; break }
    if (Has-Session) { Say "Ya hay una sesion en la distro; se va a sobrescribir." Yellow }

    $full = (Resolve-Path $Session).Path
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName  = 'wsl.exe'
    $psi.Arguments = "-d $Distro -u root -- /bin/sh -c `"mkdir -p /app/rootfs/data && tar -xzf - -C /app/rootfs/data && chown -R root:root /app/rootfs/data`""
    $psi.RedirectStandardInput = $true
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow  = $true

    $p = [System.Diagnostics.Process]::Start($psi)
    $bytes = [System.IO.File]::ReadAllBytes($full)
    $p.StandardInput.BaseStream.Write($bytes, 0, $bytes.Length)
    $p.StandardInput.BaseStream.Flush()
    $p.StandardInput.Close()
    $p.WaitForExit()

    if ($p.ExitCode -ne 0) { Say "Fallo al descomprimir la sesion (codigo $($p.ExitCode))." Red; break }
    if (Has-Session) { Say "Sesion instalada. Ya puedes correr: start" Green }
    else { Say "El tar se descomprimio pero no aparecio kvs.sqlitedb. Revisa el archivo." Red }
  }

  'start' {
    if (-not (Test-Distro)) { Say "No esta instalada la distro. Corre: install" Red; break }
    if (-not (Has-Session)) { Say "No hay sesion guardada. Corre: login" Yellow; break }
    Say "Arrancando el wrapper con la sesion existente (sin re-login)..."
    Start-Wrapper $null $null | Out-Null
  }

  'login' {
    if (-not (Test-Distro)) { Say "No esta instalada la distro. Corre: install" Red; break }
    if (Has-Session) {
      Say "Ya hay sesion guardada; no hace falta re-loguear. Usa: start" Green
      break
    }
    $u = Read-Host "   Apple ID (correo)"
    $sec = Read-Host "   Contrasena" -AsSecureString
    $pw = [Runtime.InteropServices.Marshal]::PtrToStringAuto(
            [Runtime.InteropServices.Marshal]::SecureStringToBSTR($sec))
    Start-Wrapper $u $pw | Out-Null
  }

  'status' {
    if (-not (Test-Wsl)) { break }
    Say ("Distro instalada : " + (Test-Distro))
    if (Test-Distro) {
      Say ("Sesion guardada  : " + (Has-Session))
      Say ("Wrapper corriendo: " + (Wrapper-Running))
      foreach ($p in $Ports) {
        $ok = (Test-NetConnection -ComputerName 127.0.0.1 -Port $p -WarningAction SilentlyContinue).TcpTestSucceeded
        Say ("  puerto $p     : " + $(if ($ok) { 'abierto' } else { 'cerrado' }))
      }
      $acct = try { (Invoke-WebRequest -Uri 'http://127.0.0.1:30020' -TimeoutSec 3).Content } catch { $null }
      if ($acct) { Say "Cuenta: $acct" Green }
    }
  }

  'logout' {
    if (-not (Test-Distro)) { break }
    Wsl-Run "rm -f $TokenDb" | Out-Null
    Say "Sesion borrada. El proximo arranque pedira login." Green
  }

  'uninstall' {
    if (Test-Distro) { wsl.exe --terminate $Distro | Out-Null; wsl.exe --unregister $Distro }
    Say "Distro eliminada." Green
  }
}
