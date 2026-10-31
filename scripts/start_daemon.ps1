$rmux = "$env:USERPROFILE\.cargo\bin\rmux.exe"
$pipe = '\\.\pipe\rmux-agony-spike'
$p = Start-Process -FilePath $rmux -ArgumentList @('--__internal-daemon', $pipe, '--config-default', '--config-quiet') -WindowStyle Hidden -RedirectStandardOutput 'C:\Projects\AGony\daemon.out.log' -RedirectStandardError 'C:\Projects\AGony\daemon.err.log' -PassThru
Start-Sleep -Milliseconds 500
"daemon pid: $($p.Id) exited: $($p.HasExited)"
