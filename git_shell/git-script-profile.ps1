function Get-GitConfigValue {
    param(
        [string[]]$Args
    )

    try {
        return (& git config @Args 2>$null).Trim()
    } catch {
        return ''
    }
}

function Save-GitScriptProfile {
    param(
        [string]$Repository,
        [string]$RemoteUrl,
        [string]$Protocol,
        [string]$SshHost,
        [string]$RemoteName
    )

    if ($Repository) {
        & git config --global lstwinhr.defaultRepository $Repository | Out-Null
    }
    if ($RemoteUrl) {
        & git config --global lstwinhr.defaultRemoteUrl $RemoteUrl | Out-Null
    }
    if ($Protocol) {
        & git config --global lstwinhr.defaultProtocol $Protocol | Out-Null
    }
    if ($SshHost) {
        & git config --global lstwinhr.defaultSshHost $SshHost | Out-Null
    }
    if ($RemoteName) {
        & git config --global lstwinhr.defaultRemoteName $RemoteName | Out-Null
    }
}

function Get-GitScriptProfile {
    $repository = Get-GitConfigValue -Args @('--global', '--get', 'lstwinhr.defaultRepository')
    $remoteUrl = Get-GitConfigValue -Args @('--global', '--get', 'lstwinhr.defaultRemoteUrl')
    $protocol = Get-GitConfigValue -Args @('--global', '--get', 'lstwinhr.defaultProtocol')
    $sshHost = Get-GitConfigValue -Args @('--global', '--get', 'lstwinhr.defaultSshHost')
    $remoteName = Get-GitConfigValue -Args @('--global', '--get', 'lstwinhr.defaultRemoteName')

    return @{
        Repository = $repository
        RemoteUrl  = $remoteUrl
        Protocol   = $(if ($protocol) { $protocol } else { 'ssh' })
        SshHost    = $(if ($sshHost) { $sshHost } else { 'github.com' })
        RemoteName = $(if ($remoteName) { $remoteName } else { 'origin' })
    }
}
