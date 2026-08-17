param(
    [Parameter(Mandatory = $true)]
    [string] $InstallDirectory,

    [string[]] $ProfileRoots
)

$ErrorActionPreference = "Stop"

function Get-NormalizedPath {
    param([Parameter(Mandatory = $true)][string] $Path)

    return [System.IO.Path]::GetFullPath($Path).TrimEnd(
        [System.IO.Path]::DirectorySeparatorChar,
        [System.IO.Path]::AltDirectorySeparatorChar
    )
}

function Stop-InstalledProcesses {
    param([Parameter(Mandatory = $true)][string] $InstallRoot)

    $installPrefix = "$InstallRoot$([System.IO.Path]::DirectorySeparatorChar)"
    $processNames = @("serial-mcp-console.exe", "serial-mcp-server.exe")

    Get-CimInstance -ClassName Win32_Process -ErrorAction SilentlyContinue |
        Where-Object { $processNames -contains $_.Name } |
        ForEach-Object {
            $executablePath = $_.ExecutablePath
            if (-not [string]::IsNullOrWhiteSpace($executablePath)) {
                $normalizedExecutable = Get-NormalizedPath -Path $executablePath
                if ($normalizedExecutable.StartsWith(
                    $installPrefix,
                    [System.StringComparison]::OrdinalIgnoreCase
                )) {
                    Invoke-CimMethod -InputObject $_ -MethodName Terminate -ErrorAction SilentlyContinue |
                        Out-Null
                }
            }
        }
}

function Get-ProfileRoots {
    $profileList = "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList"

    Get-ChildItem -LiteralPath $profileList -ErrorAction Stop |
        ForEach-Object {
            $profileImagePath = Get-ItemPropertyValue `
                -LiteralPath $_.PSPath `
                -Name "ProfileImagePath" `
                -ErrorAction SilentlyContinue
            if (-not [string]::IsNullOrWhiteSpace($profileImagePath)) {
                [Environment]::ExpandEnvironmentVariables([string] $profileImagePath)
            }
        } |
        Sort-Object -Unique
}

function Assert-PathHasNoReparsePoint {
    param([Parameter(Mandatory = $true)][string[]] $Paths)

    foreach ($path in $Paths) {
        if (-not (Test-Path -LiteralPath $path)) {
            continue
        }

        $item = Get-Item -LiteralPath $path -Force -ErrorAction Stop
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Refusing to clean a runtime path containing a reparse point: $path"
        }
    }
}

function Remove-ContainedTree {
    param(
        [Parameter(Mandatory = $true)][string] $Path,
        [Parameter(Mandatory = $true)][string] $AllowedRoot
    )

    $normalizedPath = Get-NormalizedPath -Path $Path
    $normalizedAllowedRoot = Get-NormalizedPath -Path $AllowedRoot
    $allowedPrefix = "$normalizedAllowedRoot$([System.IO.Path]::DirectorySeparatorChar)"
    $isAllowed = $normalizedPath.Equals(
        $normalizedAllowedRoot,
        [System.StringComparison]::OrdinalIgnoreCase
    ) -or $normalizedPath.StartsWith(
        $allowedPrefix,
        [System.StringComparison]::OrdinalIgnoreCase
    )
    if (-not $isAllowed) {
        throw "Refusing to remove a path outside the runtime data allowlist: $normalizedPath"
    }

    $item = Get-Item -LiteralPath $normalizedPath -Force -ErrorAction Stop
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Refusing to follow or remove a reparse point in runtime data: $normalizedPath"
    }

    if (-not $item.PSIsContainer) {
        Remove-Item -LiteralPath $item.FullName -Force -ErrorAction Stop
        return
    }

    foreach ($child in Get-ChildItem -LiteralPath $item.FullName -Force -ErrorAction Stop) {
        Remove-ContainedTree -Path $child.FullName -AllowedRoot $normalizedAllowedRoot
    }

    # Fetch the directory again immediately before deletion and fail closed if
    # it changed into a junction or symbolic link while cleanup was running.
    $item = Get-Item -LiteralPath $normalizedPath -Force -ErrorAction Stop
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Runtime data path changed into a reparse point: $normalizedPath"
    }
    Remove-Item -LiteralPath $item.FullName -Force -ErrorAction Stop
}

function Remove-AppRuntimeData {
    param([Parameter(Mandatory = $true)][string] $ProfileRoot)

    $normalizedProfileRoot = Get-NormalizedPath -Path $ProfileRoot
    $appData = Join-Path -Path $normalizedProfileRoot -ChildPath "AppData"
    $localAppData = Join-Path -Path $appData -ChildPath "Local"
    Assert-PathHasNoReparsePoint -Paths @(
        $normalizedProfileRoot,
        $appData,
        $localAppData
    )

    # This is the complete deletion allowlist. Do not add paths supplied by
    # environment variables or command-line options: they may be user files.
    $relativeDataDirectories = @(
        "serial-mcp-server",
        "dev.serial-mcp.console"
    )

    foreach ($relativeDirectory in $relativeDataDirectories) {
        $target = Join-Path -Path $localAppData -ChildPath $relativeDirectory
        if (-not (Test-Path -LiteralPath $target)) {
            continue
        }

        $removed = $false
        for ($attempt = 1; $attempt -le 20; $attempt++) {
            try {
                Remove-ContainedTree -Path $target -AllowedRoot $target
            } catch {
                if ($_.Exception.Message -match "reparse point|outside the runtime data allowlist") {
                    throw
                }
                # WebView2 subprocesses may need a moment to release cache files.
            }
            if (-not (Test-Path -LiteralPath $target)) {
                $removed = $true
                break
            }
            Start-Sleep -Milliseconds 250
        }

        if (-not $removed) {
            throw "Unable to remove runtime data directory: $target"
        }
    }
}

$normalizedInstallDirectory = Get-NormalizedPath -Path $InstallDirectory
Stop-InstalledProcesses -InstallRoot $normalizedInstallDirectory

if ($null -eq $ProfileRoots -or $ProfileRoots.Count -eq 0) {
    $ProfileRoots = @(Get-ProfileRoots)
}

foreach ($profileRoot in $ProfileRoots) {
    Remove-AppRuntimeData -ProfileRoot $profileRoot
}
