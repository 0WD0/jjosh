# Omit from all script steps, becuase GHA prepends it.
$ErrorActionPreference = 'stop'


Write-Output '====== Output of scratchwork for "Collect actual failures" step: ======'

[xml]$junit_xml = Get-Content 'target/nextest/with-xml/junit.xml'

$actual_failures = $junit_xml.SelectNodes("//testcase[failure]") |
    ForEach-Object { "$($_.classname) $($_.name)" } |
    Sort-Object

Write-Output $actual_failures
Set-Content -Path 'actual-failures.txt' -Value $actual_failures


Write-Output '====== Output of scratchwork for "Collect expected failures" step: ======'

$issue = 1358  # https://github.com/GitoxideLabs/gitoxide/issues/1358

$match_info = gh issue --repo GitoxideLabs/gitoxide view $issue --json body --jq .body |
    Out-String |
    Select-String -Pattern '(?s)```text\r?\n(.*?)```'

$expected_failures = $match_info.Matches.Groups[1].Value -split "`n" |
    Where-Object { ($_ -match '^\s*FAIL \[') -and ($_ -notmatch '\bperformance\b') } |
    ForEach-Object { $_ -replace '^\s*FAIL \[\s*\d+\.\d+s\]\s*', '' -replace '\s+$', '' } |
    Sort-Object

Write-Output $expected_failures
Set-Content -Path 'expected-failures.txt' -Value $expected_failures


Write-Output '====== Output of scratchwork for "Compare expected and actual failures" step: ======'

# Fail the check if there are any differences, even unexpectedly passing tests, so they can be
# investigated. (If this check is made blocking for PRs, this exact check may need to be changed.)
git --no-pager diff --no-index --exit-code -U1000000 -- expected-failures.txt actual-failures.txt


# Omit from script steps, because GHA appends it.
if ((Test-Path -LiteralPath variable:\LASTEXITCODE)) { exit $LASTEXITCODE }
