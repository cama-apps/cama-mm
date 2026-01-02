# PowerShell script to run tests
# Usage: .\run_tests.ps1 [pytest arguments]
# Examples:
#   .\run_tests.ps1                      # Run all tests in parallel
#   .\run_tests.ps1 -v                   # Run with verbose output
#   .\run_tests.ps1 -k "betting"         # Run tests matching pattern
#   .\run_tests.ps1 tests/test_betting_service.py  # Run specific test file

param(
    [Parameter(ValueFromRemainingArguments=$true)]
    [string[]]$PytestArgs
)

Write-Host "=== Cama Shuffle Test Runner ===" -ForegroundColor Cyan
Write-Host ""

# Check if uv is available
$uvAvailable = $false
try {
    $null = Get-Command uv -ErrorAction Stop
    $uvAvailable = $true
    Write-Host "Found 'uv' package manager" -ForegroundColor Green
} catch {
    Write-Host "'uv' not found, will try direct pytest" -ForegroundColor Yellow
}

# If uv is available, use it
if ($uvAvailable) {
    # Check if .venv exists, if not create it
    if (-not (Test-Path ".venv")) {
        Write-Host "Virtual environment not found, creating..." -ForegroundColor Yellow
        Write-Host "    Running: uv venv" -ForegroundColor Gray
        & uv venv
        if ($LASTEXITCODE -ne 0) {
            Write-Host "Failed to create virtual environment" -ForegroundColor Red
            exit 1
        }
        Write-Host "Virtual environment created" -ForegroundColor Green
        
        Write-Host "Installing dependencies..." -ForegroundColor Yellow
        Write-Host "    Running: uv sync" -ForegroundColor Gray
        & uv sync
        if ($LASTEXITCODE -ne 0) {
            Write-Host "Failed to install dependencies" -ForegroundColor Red
            exit 1
        }
        Write-Host "Dependencies installed" -ForegroundColor Green
    }
    
    # Build pytest command with arguments
    $pytestCmd = "pytest"
    if ($PytestArgs.Count -eq 0) {
        # Default to parallel execution if no args provided
        $pytestCmd += " -n auto"
        Write-Host "Running tests in parallel mode (-n auto)" -ForegroundColor Cyan
    } else {
        $argsString = $PytestArgs -join " "
        $pytestCmd += " " + $argsString
        Write-Host "Running tests with arguments: $argsString" -ForegroundColor Cyan
    }
    
    Write-Host ""
    Write-Host "Running: uv run $pytestCmd" -ForegroundColor Gray
    Write-Host ""
    
    # Run tests via uv
    $uvCmd = "uv run $pytestCmd"
    Invoke-Expression $uvCmd
    exit $LASTEXITCODE
}

# Fallback: Try to use virtual environment directly
Write-Host ""
Write-Host "Attempting to use virtual environment directly..." -ForegroundColor Cyan

# Check if .venv exists
if (-not (Test-Path ".venv")) {
    Write-Host ""
    Write-Host "Virtual environment not found at '.venv'" -ForegroundColor Red
    Write-Host ""
    Write-Host "Please set up the environment first:" -ForegroundColor Yellow
    Write-Host "  Option 1 (Recommended): Install uv and run this script again" -ForegroundColor White
    Write-Host "    powershell -ExecutionPolicy ByPass -c ""irm https://astral.sh/uv/install.ps1 | iex""" -ForegroundColor Gray
    Write-Host "    (Then restart PowerShell and run .\run_tests.ps1)" -ForegroundColor Gray
    Write-Host ""
    Write-Host "  Option 2: Create virtual environment manually" -ForegroundColor White
    Write-Host "    python -m venv .venv" -ForegroundColor Gray
    Write-Host "    .\.venv\Scripts\Activate.ps1" -ForegroundColor Gray
    Write-Host "    pip install -e .[dev]" -ForegroundColor Gray
    Write-Host ""
    exit 1
}

# Check if pytest is available in venv
$venvPytest = ".\.venv\Scripts\pytest.exe"
if (-not (Test-Path $venvPytest)) {
    Write-Host ""
    Write-Host "pytest not found in virtual environment" -ForegroundColor Red
    Write-Host ""
    Write-Host "Please install dependencies:" -ForegroundColor Yellow
    Write-Host "  .\.venv\Scripts\Activate.ps1" -ForegroundColor Gray
    Write-Host "  pip install -e .[dev]" -ForegroundColor Gray
    Write-Host ""
    exit 1
}

Write-Host "Found pytest in virtual environment" -ForegroundColor Green

# Build pytest command with arguments
$pytestCmd = $venvPytest
if ($PytestArgs.Count -eq 0) {
    # Default to parallel execution if no args provided
    $pytestCmd += " -n auto"
    Write-Host "Running tests in parallel mode (-n auto)" -ForegroundColor Cyan
} else {
    $argsString = $PytestArgs -join " "
    $pytestCmd += " " + $argsString
    Write-Host "Running tests with arguments: $argsString" -ForegroundColor Cyan
}

Write-Host ""
Write-Host "Running: $pytestCmd" -ForegroundColor Gray
Write-Host ""

# Run tests
& $pytestCmd
exit $LASTEXITCODE
