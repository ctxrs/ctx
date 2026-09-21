// The only retained stable transition. This is not a channel/version registry.
export const FROZEN_BRIDGE_VERSION = "1.3.2";
export const FROZEN_BRIDGE_METADATA_URL =
  "https://cli.ctx.rs/functions/v1/releases/stable/1.3.2/ctx-release-metadata.env";

export const CLI_INSTALL_SHELL_VERSION_COMPARE = `compare_release_versions() {
  LC_ALL=C awk -v left="$1" -v right="$2" '
    function numeric(s) { return s ~ /^[0-9]+$/ }
    function number(s) { return numeric(s) && (s == "0" || s !~ /^0/) }
    function cmp(a,b) {
      if (length(a) != length(b)) return length(a) < length(b) ? -1 : 1
      return ("x" a) == ("x" b) ? 0 : (("x" a) < ("x" b) ? -1 : 1)
    }
    function parse(s,parts, p,n,i,build,pre,core) {
      if (length(s) > 128 || s ~ /[^0-9A-Za-z.+-]/) return 0
      p=index(s,"+")
      if (p) {
        build=substr(s,p+1); s=substr(s,1,p-1)
        n=split(build,parts,".")
        for(i=1;i<=n;i++) if(parts[i] !~ /^[0-9A-Za-z-]+$/) return 0
        if (!n) return 0
      }
      p=index(s,"-"); pre=""
      if(p) { pre=substr(s,p+1); s=substr(s,1,p-1); if(pre == "") return 0 }
      n=split(s,core,"."); if(n != 3) return 0
      for(i=1;i<=3;i++) if(!number(core[i])) return 0
      parts[1]=core[1]; parts[2]=core[2]; parts[3]=core[3]; parts[4]=pre
      if(pre != "") {
        n=split(pre,core,".")
        for(i=1;i<=n;i++) if(core[i] !~ /^[0-9A-Za-z-]+$/ || (numeric(core[i]) && !number(core[i]))) return 0
      }
      return 1
    }
    BEGIN {
      if(!parse(left,a) || !parse(right,b)) exit 2
      result=0
      for(i=1;i<=3;i++) { result=cmp(a[i],b[i]); if(result) break }
      if(!result && a[4] != b[4]) {
        if(a[4] == "") result=1
        else if(b[4] == "") result=-1
        else {
          na=split(a[4],ap,"."); nb=split(b[4],bp,".")
          for(i=1;i<=na && i<=nb;i++) {
            if(numeric(ap[i]) && numeric(bp[i])) result=cmp(ap[i],bp[i])
            else if(numeric(ap[i]) != numeric(bp[i])) result=numeric(ap[i]) ? -1 : 1
            else result=("x" ap[i]) == ("x" bp[i]) ? 0 : (("x" ap[i]) < ("x" bp[i]) ? -1 : 1)
            if(result) break
          }
          if(!result && na != nb) result=na < nb ? -1 : 1
        }
      }
      print result
    }
  '
}`;

export const CLI_INSTALL_POWERSHELL_VERSION_COMPARE = `function Compare-ReleaseVersion([string]$Left, [string]$Right) {
    $parsed = @()
    foreach ($value in @($Left, $Right)) {
        if ($value.Length -gt 128 -or $value -cnotmatch '^(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)(?:-([0-9A-Za-z-]+(?:\\.[0-9A-Za-z-]+)*))?(?:\\+[0-9A-Za-z-]+(?:\\.[0-9A-Za-z-]+)*)?$') {
            Fail "invalid release version: $value"
        }
        $entry = @($Matches[1], $Matches[2], $Matches[3], [string]$Matches[4])
        foreach ($identifier in $entry[3].Split('.')) {
            if ($identifier -cmatch '^0[0-9]+$') { Fail "invalid release version: $value" }
        }
        $parsed += ,$entry
    }
    for ($i = 0; $i -lt 3; $i++) {
        $a = $parsed[0][$i]; $b = $parsed[1][$i]
        if ($a.Length -ne $b.Length) { return [Math]::Sign($a.Length - $b.Length) }
        $order = [string]::CompareOrdinal($a, $b)
        if ($order -ne 0) { return [Math]::Sign($order) }
    }
    $a = $parsed[0][3]; $b = $parsed[1][3]
    if ($a -ceq $b) { return 0 }
    if ($a -ceq '') { return 1 }
    if ($b -ceq '') { return -1 }
    $leftIds = $a.Split('.'); $rightIds = $b.Split('.')
    for ($i = 0; $i -lt [Math]::Min($leftIds.Length, $rightIds.Length); $i++) {
        $a = $leftIds[$i]; $b = $rightIds[$i]
        $an = $a -cmatch '^[0-9]+$'; $bn = $b -cmatch '^[0-9]+$'
        if ($an -and $bn -and $a.Length -ne $b.Length) { return [Math]::Sign($a.Length - $b.Length) }
        if ($an -ne $bn) { if ($an) { return -1 }; return 1 }
        $order = [string]::CompareOrdinal($a, $b)
        if ($order -ne 0) { return [Math]::Sign($order) }
    }
    return [Math]::Sign($leftIds.Length - $rightIds.Length)
}`;
