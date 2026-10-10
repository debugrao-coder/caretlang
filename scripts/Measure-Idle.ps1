[CmdletBinding()]
param(
    [string]$ExePath = (Join-Path $PSScriptRoot '..\target\release\caretlang.exe'),
    [ValidateRange(1, 86400)][int]$Seconds = 300,
    [string]$OutputPath = (Join-Path $PSScriptRoot '..\artifacts\idle.json')
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$expectedPath = [IO.Path]::GetFullPath($ExePath)
$outputFile = [IO.Path]::GetFullPath($OutputPath)
$processorCount = [Environment]::ProcessorCount
$tracked = @{}
$problems = [Collections.Generic.List[string]]::new()
$samples = [Collections.Generic.List[object]]::new()
$watch = [Diagnostics.Stopwatch]::StartNew()
$started = [DateTimeOffset]::Now.ToString('o')
$parentId = 0
$cpuSeconds = 0.0
$peakPrivateBytes = 0L
$startPrivateBytes = 0L
$endPrivateBytes = 0L

function Get-ExactProcesses {
    $name = [IO.Path]::GetFileNameWithoutExtension($expectedPath)
    @(Get-Process -Name $name -ErrorAction SilentlyContinue | Where-Object {
        try { [string]::Equals($_.Path, $expectedPath, [StringComparison]::OrdinalIgnoreCase) }
        catch { $false }
    })
}

try {
    if (-not (Test-Path -LiteralPath $expectedPath -PathType Leaf)) { throw "Executable does not exist: $expectedPath" }
    $initial = @(Get-ExactProcesses)
    if ($initial.Count -eq 0) { throw 'No exact-path CaretLang process is running; start it before measuring.' }
    foreach ($process in $initial) {
        $info = Get-CimInstance Win32_Process -Filter "ProcessId=$($process.Id)"
        if ($null -eq $info -or [string]::IsNullOrEmpty($info.CommandLine)) { throw "Cannot inspect process $($process.Id)." }
        if ($info.CommandLine -notmatch '(?i)(?:^|\s)--uia-worker(?:\s|$)') {
            if ($parentId -ne 0) { throw 'More than one exact-path main process is running.' }
            $parentId = $process.Id
        }
    }
    if ($parentId -eq 0) { throw 'No main CaretLang process found; only worker processes are running.' }
    $first = $true
    while ($true) {
        $current = @(Get-ExactProcesses)
        $seen = @{}
        $totalPrivate = 0L
        foreach ($process in $current) {
            $id = $process.Id
            $process.Refresh()
            $startTicks = $process.StartTime.ToUniversalTime().Ticks
            $key = "$id/$startTicks"
            $seen[$key] = $true
            $cpu = $process.TotalProcessorTime.TotalSeconds
            $private = $process.PrivateMemorySize64
            if (-not $tracked.ContainsKey($key)) {
                $info = Get-CimInstance Win32_Process -Filter "ProcessId=$id"
                if ($id -eq $parentId) { $role = 'main' }
                elseif ($null -ne $info -and $info.ParentProcessId -eq $parentId -and $info.CommandLine -match '(?i)(?:^|\s)--uia-worker(?:\s|$)') { $role = 'uia-worker' }
                else { throw "Unexpected exact-path process $id; cannot attribute it to this app instance." }
                $tracked[$key] = [pscustomobject]@{ pid = $id; role = $role; start_ticks = $startTicks; last_cpu_seconds = $cpu; exited = $false }
                if (-not $first) { $cpuSeconds += $cpu }
            }
            else {
                $cpuSeconds += [Math]::Max(0.0, $cpu - $tracked[$key].last_cpu_seconds)
                $tracked[$key].last_cpu_seconds = $cpu
            }
            $totalPrivate += $private
        }
        foreach ($key in @($tracked.Keys)) {
            if (-not $seen.ContainsKey($key) -and -not $tracked[$key].exited) {
                $tracked[$key].exited = $true
                $problems.Add("Process $($tracked[$key].pid) ($($tracked[$key].role)) exited during measurement; CPU result may be incomplete.")
            }
        }
        if ($first) {
            $startPrivateBytes = $totalPrivate
            $watch.Restart()
            $first = $false
        }
        $endPrivateBytes = $totalPrivate
        $peakPrivateBytes = [Math]::Max($peakPrivateBytes, $totalPrivate)
        $samples.Add([pscustomobject]@{ elapsed_seconds = [Math]::Round($watch.Elapsed.TotalSeconds, 3); private_bytes = $totalPrivate; process_count = $current.Count })
        if (-not ($current.Id -contains $parentId)) { throw 'Main process exited during measurement.' }
        if ($watch.Elapsed.TotalSeconds -ge $Seconds) { break }
        $remaining = $Seconds - $watch.Elapsed.TotalSeconds
        Start-Sleep -Milliseconds ([int][Math]::Max(1, [Math]::Min(1000, $remaining * 1000)))
    }
}
catch { $problems.Add($_.Exception.Message) }
finally {
    $watch.Stop()
    $elapsed = $watch.Elapsed.TotalSeconds
    $rawCpu = if ($elapsed -gt 0) { 100.0 * $cpuSeconds / $elapsed } else { 0.0 }
    $normalizedCpu = $rawCpu / $processorCount
    $complete = $elapsed -ge $Seconds -and $problems.Count -eq 0
    $cpuPass = $complete -and $normalizedCpu -le 0.2
    $memoryPass = $complete -and $peakPrivateBytes -le 60MB
    $report = [ordered]@{
        schema_version = 1; started_at = $started; finished_at = [DateTimeOffset]::Now.ToString('o')
        executable = $expectedPath; requested_seconds = $Seconds; elapsed_seconds = [Math]::Round($elapsed, 3)
        executable_sha256 = $(if (Test-Path -LiteralPath $expectedPath -PathType Leaf) { (Get-FileHash -LiteralPath $expectedPath -Algorithm SHA256).Hash.ToLowerInvariant() } else { $null })
        logical_processors = $processorCount; cpu_seconds = [Math]::Round($cpuSeconds, 6)
        cpu_percent_one_core = [Math]::Round($rawCpu, 6); cpu_percent_machine = [Math]::Round($normalizedCpu, 6)
        private_bytes_start = $startPrivateBytes; private_bytes_end = $endPrivateBytes; private_bytes_peak = $peakPrivateBytes
        criteria = @{ cpu_percent_machine_max = 0.2; private_bytes_peak_max = 60MB }
        cpu_pass = $cpuPass; memory_pass = $memoryPass; status = $(if ($cpuPass -and $memoryPass) { 'PASS' } else { 'FAIL' })
        processes = @($tracked.Values | Select-Object pid, role, exited)
        errors = @($problems.ToArray()); samples = @($samples.ToArray())
        limitation = 'Passive measurement only. Idle input state is not enforced. Sampled memory peak can miss subsecond peaks. Exited processes invalidate the run.'
    }
    $directory = [IO.Path]::GetDirectoryName($outputFile)
    [IO.Directory]::CreateDirectory($directory) | Out-Null
    $report | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $outputFile -Encoding utf8
    Write-Output "$($report.status): CPU=$($report.cpu_percent_machine)% machine / $($report.cpu_percent_one_core)% one core; peak private=$peakPrivateBytes bytes; report=$outputFile"
}
if ($report.status -ne 'PASS') { exit 1 }
