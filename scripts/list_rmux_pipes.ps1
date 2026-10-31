$pipes = [IO.Directory]::GetFiles("\\.\pipe\") | Where-Object { $_ -match 'rmux' }
if ($pipes) { $pipes } else { "no rmux pipes found" }
