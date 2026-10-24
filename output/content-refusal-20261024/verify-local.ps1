$ErrorActionPreference = 'Stop'
$imageArgs = @('image','inspect','aether-app:latest','--format','{{.Id}}')
$expectedImage = (& docker @imageArgs).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Cannot inspect the built image' }
$inspectArgs = @('inspect','aether-app','--format','{{json .}}')
$container = (& docker @inspectArgs | ConvertFrom-Json)
if ($LASTEXITCODE -ne 0) { throw 'Cannot inspect the application container' }
if ($container.Image -ne $expectedImage) { throw 'Application container is using a different image' }
if ($container.State.Health.Status -ne 'healthy') { throw 'Application container is not healthy' }
$baseUrl = 'http://127.0.0.1:8084'
foreach ($path in @('/health','/readyz','/')) {
    $response = Invoke-WebRequest -Uri ($baseUrl + $path) -NoProxy -TimeoutSec 15
    if ($response.StatusCode -ne 200) { throw "Unexpected status for $path" }
    [pscustomobject]@{ Path = $path; Status = $response.StatusCode; ContentType = $response.Headers['Content-Type'] -join ',' } | ConvertTo-Json -Compress
}
$buildLog = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'local-build.log') -Raw
$usageAsset = [regex]::Match($buildLog, 'dist/assets/(Usage-[A-Za-z0-9_-]+\.js)').Groups[1].Value
if (-not $usageAsset) { throw 'Cannot find the new usage bundle in build output' }
$asset = Invoke-WebRequest -Uri ($baseUrl + '/assets/' + $usageAsset) -NoProxy -TimeoutSec 15
if ($asset.Content -notmatch 'content_refused') { throw 'The served usage bundle is missing content refusal handling' }
[pscustomobject]@{
    Image = $expectedImage
    Container = $container.Name.TrimStart('/')
    Health = $container.State.Health.Status
    StartedAt = $container.State.StartedAt
    UsageAsset = $usageAsset
    ContentRefusalCodePresent = $true
} | ConvertTo-Json -Compress
