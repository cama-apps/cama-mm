# PowerShell script to create .env file
# Run this in PowerShell: .\create_env.ps1

$envContent = @"
# Discord Bot Token
# Get this from https://discord.com/developers/applications
DISCORD_BOT_TOKEN=your_bot_token_here
# Admin allowlist for commands like /addfake (comma-separated Discord user IDs)
# Example: 123456789012345678,234567890123456789
ADMIN_USER_IDS=
"@

$envFile = ".env"

if (Test-Path $envFile) {
    Write-Host ".env file already exists!" -ForegroundColor Yellow
    $overwrite = Read-Host "Do you want to overwrite it? (y/n)"
    if ($overwrite -ne "y") {
        Write-Host "Cancelled." -ForegroundColor Red
        exit
    }
}

Set-Content -Path $envFile -Value $envContent
Write-Host ".env file created successfully!" -ForegroundColor Green
Write-Host "Please edit .env and add your Discord bot token." -ForegroundColor Yellow


