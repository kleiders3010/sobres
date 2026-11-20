# Signs a release AAB (Google Play upload) with the release key (~/jurisgames-signing), reading the
# keystore password from Credential Manager into a process-local env var; it is never printed.
# Usage: powershell -File scripts\sign-bundle.ps1 <unsigned.aab> <signed.aab>
param([Parameter(Mandatory)][string]$In, [Parameter(Mandatory)][string]$Out)
$ErrorActionPreference = 'Stop'
$jarsigner = Join-Path $env:JAVA_HOME 'bin\jarsigner.exe'
if (-not (Test-Path $jarsigner)) { $jarsigner = (Get-Command jarsigner).Source }
$jks = Join-Path $HOME 'jurisgames-signing\jurisgames-tasks-release.jks'
Add-Type -Namespace Win32 -Name CredB -MemberDefinition @'
[StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
public struct CREDENTIAL { public int Flags; public int Type; public string TargetName; public string Comment; public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten; public int CredentialBlobSize; public IntPtr CredentialBlob; public int Persist; public int AttributeCount; public IntPtr Attributes; public string TargetAlias; public string UserName; }
[DllImport("advapi32.dll", SetLastError = true, CharSet = CharSet.Unicode)] public static extern bool CredRead(string target, int type, int flags, out IntPtr cred);
[DllImport("advapi32.dll")] public static extern void CredFree(IntPtr cred);
'@
$p = [IntPtr]::Zero
if (-not [Win32.CredB]::CredRead('jurisgames-tasks-release-keystore', 1, 0, [ref]$p)) { throw 'Keystore credential not found.' }
try {
  $c = [System.Runtime.InteropServices.Marshal]::PtrToStructure($p, [type][Win32.CredB+CREDENTIAL])
  $env:KT_PW = [System.Runtime.InteropServices.Marshal]::PtrToStringUni($c.CredentialBlob, $c.CredentialBlobSize / 2)
} finally { [Win32.CredB]::CredFree($p) }
try {
  Copy-Item $In $Out -Force
  $ErrorActionPreference = 'Continue'
  & $jarsigner -keystore $jks -storepass:env KT_PW -keypass:env KT_PW -digestalg SHA-256 $Out tasks | Out-Null
  if ($LASTEXITCODE -ne 0) { throw "jarsigner failed ($LASTEXITCODE)" }
} finally { Remove-Item Env:KT_PW -ErrorAction SilentlyContinue }
& $jarsigner -verify $Out | Select-Object -First 1
