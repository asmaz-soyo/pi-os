# フェーズ2-72 自作 TCP(2):シーケンス番号と接続の状態遷移

## 何をしたか

- `net/tcp/seq.rs`:`Seq`。番号は 32 ビットで一周するので、`a < b` は「差 `a - b` を符号付き(i32)にして負か」で決める。2 つの番号の差が半周(2^31)より小さいあいだは正しい。ウィンドウは高々 64KiB なので十分
- `net/tcp/conn.rs`:`Conn`(TCB。1 本の接続の状態)。ハードウェアにも時計にも触らない純粋なロジックで、
  - `on_segment(&Segment)`:届いたものを処理する
  - `dispatch() -> Vec<Vec<u8>>`:送るべきものを取り出す
  - `send` / `recv` / `close`:ソケットから使う口
  ホストのテストでは、2 つの `Conn` を「線」でつなぎ(片方の `dispatch` をもう片方の `on_segment` に渡す)、送るものがなくなるまで回す

## 送る側の変数(RFC 9293 の 3.3.1)

```text
         snd_una            snd_nxt          snd_una + snd_wnd
            |                  |                    |
  確認済み  |  送ったが未確認   |  送ってよい(未送信) |  まだ送れない
```

- 送るデータは `send_buf` に、確認されるまで持っておく(先頭の番号が `buf_seq`)。ACK が来たら、その分を捨てる
- `dispatch` は「`snd_nxt` から、データの終わりとウィンドウの端の手前まで、相手の MSS ずつ」送る。SYN は `snd_nxt == iss` のときに、FIN は全データのあとに送る
- だから再送(次のステップ)は「`snd_nxt` を `snd_una` に戻す」だけで、同じ道で送り直せる

## 受け取る側

- `rcv_nxt`:次に受け取るはずの番号。これより先の番号のセグメント(あいだが抜けている)は捨て、「まだ `rcv_nxt` を待っている」という ACK を返す(重複 ACK)。並べ直しはしない(相手が再送する)
- 前に受け取った部分と重なるセグメント(再送)は、重なりを切り落として残りだけ使う
- ウィンドウ = 受け取るバッファの空き。読まれてバッファが空いたら、知らせていたウィンドウが小さかった場合だけ ACK でそれを知らせる(相手は窓が 0 なら送るのを止めている)

## 状態遷移

```text
connect → SYN-SENT ──SYN+ACK──→ ESTABLISHED ←──ACK── SYN-RECEIVED ← SYN(accept)
ESTABLISHED ─close→ FIN-WAIT-1 ─ACK→ FIN-WAIT-2 ─FIN→ TIME-WAIT
                    FIN-WAIT-1 ─FIN→ CLOSING ─ACK→ TIME-WAIT        (同時に閉じる)
ESTABLISHED ─FIN→ CLOSE-WAIT ─close→ LAST-ACK ─ACK→ CLOSED
```

- SYN と FIN も番号を 1 つ使うので、ACK で確認される。「FIN が確認された」は「データを全部捨てたあと、`snd_una` がその次の番号まで来た」こと
- RST:SYN-SENT で、こちらの SYN への答えが RST なら ECONNREFUSED。つながったあとは、番号がウィンドウの中のときだけ ECONNRESET(外れた RST は、なりすましかもしれないので無視)
- まだ送っていない番号への ACK は無視し、ACK を返す

## 確かめたこと(ホスト)

ハンドシェイクの 3 つのセグメントの中身、両方向のデータ、MSS で切る、ウィンドウで止まり読むと再開、送るバッファの上限、能動・受動・同時のクローズ、RST(拒否・リセット・窓の外)、順番が抜けたもの・重なったもの・重複、送っていない番号への ACK

## まだのこと

- 再送(タイマー)、TIME-WAIT の時間切れ、ウィンドウ 0 のときの問い合わせ(次のステップ)
- `Stack` へのつなぎ込み
