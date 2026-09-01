$TargetTitles = @($env:DRIVER_TUI_TITLES_JSON | ConvertFrom-Json)
$Session = New-Object -ComObject Microsoft.Update.Session
$Searcher = $Session.CreateUpdateSearcher()
$Searcher.ServerSelection = 2
$Results = $Searcher.Search("IsInstalled=0 and Type='Driver'")

$Total = $TargetTitles.Count
$GlobalReboot = $false

for ($i = 0; $i -lt $Total; $i++) {
    $Target = $TargetTitles[$i]
    $UpdateMatch = $null
    foreach ($U in $Results.Updates) {
        if ($U.Title -eq $Target) {
            $UpdateMatch = $U
            break
        }
    }

    if (-not $UpdateMatch) {
        $Evt = @{ "type" = "ItemFinished"; "data" = @{ "index" = $i; "title" = $Target; "success" = $false; "reboot_required" = $false; "message" = "Update package not found in catalog" } } | ConvertTo-Json -Compress
        [Console]::WriteLine($Evt)
        continue
    }

    # 1. Download Step
    $Evt = @{ "type" = "ItemDownloading"; "data" = @{ "index" = $i; "title" = $Target } } | ConvertTo-Json -Compress
    [Console]::WriteLine($Evt)

    $DownColl = New-Object -ComObject Microsoft.Update.UpdateColl
    $DownColl.Add($UpdateMatch) | Out-Null
    $Downloader = $Session.CreateUpdateDownloader()
    $Downloader.Updates = $DownColl

    $DownloadFailed = $false
    $DownloadErrMsg = ""
    try {
        $DownResult = $Downloader.Download()
        if ($DownResult.ResultCode -ne 2 -and $DownResult.ResultCode -ne 3) {
            $DownloadFailed = $true
            $DownloadErrMsg = "Download operation failed with result code $($DownResult.ResultCode)"
        }
    } catch {
        $DownloadFailed = $true
        $DownloadErrMsg = "Download failed: $($_.Exception.Message)"
    }

    if ($DownloadFailed) {
        $Evt = @{ "type" = "ItemFinished"; "data" = @{ "index" = $i; "title" = $Target; "success" = $false; "reboot_required" = $false; "message" = $DownloadErrMsg } } | ConvertTo-Json -Compress
        [Console]::WriteLine($Evt)
        continue
    }

    # 2. Installation Step
    $Evt = @{ "type" = "ItemInstalling"; "data" = @{ "index" = $i; "title" = $Target } } | ConvertTo-Json -Compress
    [Console]::WriteLine($Evt)

    $InstColl = New-Object -ComObject Microsoft.Update.UpdateColl
    $InstColl.Add($UpdateMatch) | Out-Null
    $Installer = $Session.CreateUpdateInstaller()
    $Installer.Updates = $InstColl
    try {
        $InstResult = $Installer.Install()
        $Success = ($InstResult.ResultCode -eq 2 -or $InstResult.ResultCode -eq 3)
        $Reboot = [bool]$InstResult.RebootRequired
        if ($Reboot) { $GlobalReboot = $true }

        $Msg = switch ($InstResult.ResultCode) {
            2 { if ($Reboot) { "Installed successfully (reboot pending)" } else { "Installed successfully" } }
            3 { if ($Reboot) { "Installed with warnings (reboot pending)" } else { "Installed with warnings" } }
            4 { "Installation failed by system" }
            5 { "Installation canceled" }
            Default { "Completed with code $($InstResult.ResultCode)" }
        }

        $Evt = @{ "type" = "ItemFinished"; "data" = @{ "index" = $i; "title" = $Target; "success" = $Success; "reboot_required" = $Reboot; "message" = $Msg } } | ConvertTo-Json -Compress
        [Console]::WriteLine($Evt)
    } catch {
        $Evt = @{ "type" = "ItemFinished"; "data" = @{ "index" = $i; "title" = $Target; "success" = $false; "reboot_required" = $false; "message" = "Install error: $($_.Exception.Message)" } } | ConvertTo-Json -Compress
        [Console]::WriteLine($Evt)
    }
}

$Evt = @{ "type" = "AllFinished"; "data" = @{ "reboot_required" = $GlobalReboot } } | ConvertTo-Json -Compress
[Console]::WriteLine($Evt)
