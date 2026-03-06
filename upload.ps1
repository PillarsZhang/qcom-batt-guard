$ErrorActionPreference = "Stop"

$Remote = "mi5ubuntu"
$RemoteDir = "~/workspace/2026-03-06_qcom-batt-guard/qcom-batt-guard"

$Items = @(
    "Cargo.toml",
    "Cargo.lock",
    "src",
    "systemd",
    "install.sh",
    "uninstall.sh",
    "README.md"
)

$DeleteCmd = ($Items | ForEach-Object { "rm -rf $RemoteDir/$_" }) -join "; "
$RemoteCmd = "$DeleteCmd; mkdir -p $RemoteDir; tar -xzvf - -C $RemoteDir"

Write-Host "Remote command: $RemoteCmd"

tar -czf - $Items | ssh $Remote $RemoteCmd

Write-Host "Upload completed successfully."
