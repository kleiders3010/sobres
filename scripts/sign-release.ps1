# Signs a release APK with the release key (~/jurisgames-signing), reading the keystore
# password from Credential Manager into a process-local env var; it is never printed.
# Usage: powershell -File scripts\sign-release.ps1 <unsigned.apk> <signed.apk>
param([Parameter(Mandatory)][string]$In, [Parameter(Mandatory)][string]$Out)
$ErrorActionPreference = 'Stop'
$apksigner = Join-Path $env:LOCALAPPDATA 'Android\Sdk\build-tools\36.0.0\apksigner.bat'
$jks = Join-Path $HOME 'jurisgames-signing\jurisgames-tasks-release.jks'
Add-Type -Namespace Win32 -Name CredS -MemberDefinition @'
[StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
public struct CREDENTIAL { public int Flags; public int Type; public string TargetName; public string Comment; public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten; public int CredentialBlobSize; public IntPtr CredentialBlob; public int Persist; public int AttributeCount; public IntPtr Attributes; public string TargetAlias; public string UserName; }
[DllImport("advapi32.dll", SetLastError = true, CharSet = CharSet.Unicode)] public static extern bool CredRead(string target, int type, int flags, out IntPtr cred);
[DllImport("advapi32.dll")] public static extern void CredFree(IntPtr cred);
'@
$p = [IntPtr]::Zero
if (-not [Win32.CredS]::CredRead('jurisgames-tasks-release-keystore', 1, 0, [ref]$p)) { throw 'Keystore credential not found.' }
try {
  $c = [System.Runtime.InteropServices.Marshal]::PtrToStructure($p, [type][Win32.CredS+CREDENTIAL])
  $env:KT_PW = [System.Runtime.InteropServices.Marshal]::PtrToStringUni($c.CredentialBlob, $c.CredentialBlobSize / 2)
} finally { [Win32.CredS]::CredFree($p) }
try {
  $ErrorActionPreference = 'Continue'
  & $apksigner sign --ks $jks --ks-key-alias tasks --ks-pass env:KT_PW --key-pass env:KT_PW --out $Out $In
  if ($LASTEXITCODE -ne 0) { throw "apksigner sign failed ($LASTEXITCODE)" }
} finally { Remove-Item Env:KT_PW -ErrorAction SilentlyContinue }
& $apksigner verify --print-certs $Out | Select-String 'SHA-256'
