$Session = New-Object -ComObject Microsoft.Update.Session
$Searcher = $Session.CreateUpdateSearcher()
$Searcher.ServerSelection = 2

# 1. Read reboot-pending GUIDs from registry
$RebootGuids = @{}
$RebootKeyPath = "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired"
if (Test-Path $RebootKeyPath) {
    $Props = (Get-ItemProperty $RebootKeyPath).PSObject.Properties
    foreach ($P in $Props) {
        if ($P.Name -notmatch '^PS.*') {
            $RebootGuids[$P.Name.ToLower()] = $true
        }
    }
}

[System.Collections.ArrayList]$Pending = @()
[System.Collections.ArrayList]$Staged = @()
$ProcessedUpdateIds = @{}

# 2. Search uninstalled catalog drivers
try {
    $Results = $Searcher.Search("IsInstalled=0 and Type='Driver'")
    foreach ($Update in $Results.Updates) {
        $UpdateId = $Update.Identity.UpdateID.ToLower()
        $IsReboot = ($Update.RebootRequired -or $RebootGuids.ContainsKey($UpdateId))

        if ($IsReboot) {
            if (-not $ProcessedUpdateIds.ContainsKey($UpdateId)) {
                $ProcessedUpdateIds[$UpdateId] = $true
                [void]$Staged.Add([PSCustomObject]@{ title = $Update.Title })
            }
        } else {
            if (-not $ProcessedUpdateIds.ContainsKey($UpdateId)) {
                $ProcessedUpdateIds[$UpdateId] = $true
                [void]$Pending.Add([PSCustomObject]@{
                    Title           = $Update.Title
                    UpdateId        = $Update.Identity.UpdateID
                    MaxDownloadSize = $Update.MaxDownloadSize
                    DriverClass     = $Update.DriverClass
                    DriverProvider  = $Update.DriverProvider
                })
            }
        }
    }
} catch {
    [Console]::Error.WriteLine("axle: catalog scan failed: $($_.Exception.Message)")
}

# 3. Fast history check: only scan if there are still unmatched reboot GUIDs
$UnmatchedCount = ($RebootGuids.Keys | Where-Object { -not $ProcessedUpdateIds.ContainsKey($_) }).Count
if ($UnmatchedCount -gt 0) {
    try {
        $HistoryCount = $Searcher.GetTotalHistoryCount()
        if ($HistoryCount -gt 0) {
            # Limit history scan depth to 40 entries for speed
            $Take = [Math]::Min($HistoryCount, 40)
            $History = $Searcher.QueryHistory(0, $Take)
            
            foreach ($Item in $History) {
                $HistId = $Item.UpdateIdentity.UpdateID.ToLower()
                if ($RebootGuids.ContainsKey($HistId) -and (-not $ProcessedUpdateIds.ContainsKey($HistId))) {
                    $IsDriver = $false
                    if ($Item.Categories) {
                        $CatMatch = $Item.Categories | Where-Object {
                            $_.Name -like "*Driver*" -or
                            $_.Name -like "*Device*" -or
                            $_.CategoryId -eq "ebfc9e0a-0498-422a-a9e9-7c40d25514f7"
                        }
                        if ($CatMatch) { $IsDriver = $true }
                    }
                    if (-not $IsDriver -and ($Item.Title -match "(?i)(driver|extension|intel|lenovo|realtek|nvidia|amd|qualcomm|audio|bluetooth|firmware|sensor)")) {
                        $IsDriver = $true
                    }

                    if ($IsDriver) {
                        $ProcessedUpdateIds[$HistId] = $true
                        [void]$Staged.Add([PSCustomObject]@{ title = $Item.Title })
                    }
                }
            }
        }
    } catch {
        [Console]::Error.WriteLine("axle: reboot history scan failed: $($_.Exception.Message)")
    }
}

[PSCustomObject]@{
    pending = @($Pending)
    staged  = @($Staged)
} | ConvertTo-Json -Compress -Depth 3