# フェーズ2-88 シグナル(3):待ちの中断、SA_RESTART、sigsuspend、SIGPIPE

## 何をしたか

- **待ちの中断**:`wait_event_intr`(`wait.rs`)は、待っているあいだにマスクされていないシグナルが来たら EINTR を返す。パイプ・FIFO を開く待ち・AF_UNIX・TCP/UDP・端末・wait4・GPIO・timerfd の待ちをこれにした。待つ側は `Process` を借りているのでマスクを知っているが、`wait_event` の中からは見えない。そこでマスクの写しをプロセスの表に置き(`Process::set_sig_blocked` が書く)、`interrupted()` はそれで判断する
- **SA_RESTART**:EINTR になったシステムコールは、番号と元の x0 を覚えておく(`Process::restart`)。ユーザーに戻る直前に:
  - SA_RESTART のハンドラを呼ぶとき、やり直せるシステムコール(read、write、wait4、accept、connect、futex など。Linux で ERESTARTSYS のもの)なら、フレームに積む前に pc を `svc` の命令(4 バイト前)に、x0 を元に戻す。ハンドラから戻ると、もう一度そのシステムコールに入る
  - ハンドラが呼ばれなかった(シグナルが無視された)ときは、いつでもやり直す
  - nanosleep・poll・sigsuspend は、ハンドラのあとはやり直さない(Linux と同じく EINTR)
- **rt_sigsuspend**:マスクを一時的に差し替えて、シグナルを待つ。ハンドラのフレームには差し替える前のマスクを残すので、ハンドラから戻ると元のマスクに戻る。シェル(ash)の `wait` はこれで SIGCHLD を待つので、`sleep 1 & wait $!` が動くようになった
- **SIGPIPE**:write / writev / pwrite / sendto / sendmsg が EPIPE になったら、そのスレッドに SIGPIPE を送る(send の MSG_NOSIGNAL は除く)

## 見つかったこと

ホストのテストのプロセスは番号 0 で、全部のテストが同じ番号を使う。TCP のテスト(つながっていないソケットに書いて EPIPE)が番号 0 に SIGPIPE を残し、並んで動く futex や nanosleep のテストの眠りが EINTR で中断された。番号 0 はカーネルが直接作ったプロセスで本物のスレッドではないので、SIGPIPE を送らないことにした

## 確かめたこと

- QEMU と互換テスト(sigwait。別のスレッドから少しあとに pthread_kill):SA_RESTART なしの read は EINTR、SA_RESTART ありはやり直して、あとから書かれた 1 バイトを読む、sigsuspend は EINTR でマスクが元に戻る、読む人のいないパイプへの write は EPIPE と SIGPIPE
- QEMU:シェルの `wait` で後ろの仕事を待ち、終了ステータスを受け取る
