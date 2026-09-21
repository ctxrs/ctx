param([string]$GuardPath, [string]$WorkRoot)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:OS -ne 'Windows_NT') { throw 'ACL fixture requires native Windows' }
. $GuardPath # Exact C# block extracted by the existing platform owner.
$owner = [Security.Principal.WindowsIdentity]::GetCurrent().User
$everyone = [Security.Principal.SecurityIdentifier]::new('S-1-1-0')
$bin = Join-Path $WorkRoot 'bin'
$null = New-Item -ItemType Directory -Path $bin
$binary = Join-Path $bin 'ctx.exe'; $marker = "$binary.install.json"
[IO.File]::WriteAllText($binary, 'inert leaf, never executed')
[IO.File]::WriteAllText($marker, '{}')
$hashes = @{}
foreach ($path in @($binary, $marker)) { $hashes[$path] = (Get-FileHash -LiteralPath $path).Hash }
$checked = 0
foreach ($path in @($bin, $binary, $marker)) {
    $directory = (Get-Item -LiteralPath $path).PSIsContainer
    $inheritance = [Security.AccessControl.InheritanceFlags]::None
    if ($directory) {
        $inheritance = [Security.AccessControl.InheritanceFlags]::ContainerInherit -bor [Security.AccessControl.InheritanceFlags]::ObjectInherit
    }
    $acl = Get-Acl -LiteralPath $path
    $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new(
        $everyone, 'FullControl', $inheritance, 'None', 'Allow'))
    Set-Acl -LiteralPath $path -AclObject $acl
    $before = (Get-Acl -LiteralPath $path).GetAccessRules($true, $false, [Security.Principal.SecurityIdentifier])
    if ('S-1-1-0' -notin $before.IdentityReference.Value) { throw 'hostile ACE setup did not apply' }
    $guard = if ($directory) { [CtxInstallerPathGuard]::AcquireDirectory($path, $false) }
        else { [CtxInstallerPathGuard]::AcquireLeaf($path) }
    try {
        [CtxInstallerAcl]::Protect($path, $directory, $owner.Value)
        $guard.AssertUnchanged()
    } finally { $guard.Dispose() }
    $acl = Get-Acl -LiteralPath $path
    if (-not $acl.AreAccessRulesProtected -or $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -cne $owner.Value) {
        throw 'managed ACL ownership/protection differs'
    }
    $rules = @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
    if ($rules.Count -ne 2) { throw 'managed ACL must contain exactly owner and SYSTEM' }
    $seen = @{}
    foreach ($rule in $rules) {
        $sid = $rule.IdentityReference.Value
        if ($sid -notin @($owner.Value, 'S-1-5-18') -or $seen.ContainsKey($sid) -or
            $rule.AccessControlType -ne 'Allow' -or $rule.FileSystemRights -ne [Security.AccessControl.FileSystemRights]::FullControl -or
            $rule.InheritanceFlags -ne $inheritance -or $rule.PropagationFlags -ne 'None' -or $rule.IsInherited) {
            throw 'managed ACL contains unexpected authority'
        }
        $seen[$sid] = $true
    }
    $checked++
}
foreach ($path in @($binary, $marker)) {
    if ((Get-FileHash -LiteralPath $path).Hash -cne $hashes[$path]) { throw 'ACL repair changed leaf bytes' }
}
[ordered]@{ checked_paths=$checked; leaves_unchanged=$true } | ConvertTo-Json -Compress
