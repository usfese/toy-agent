import atexit, http.server, json, os, pty, select, signal, struct, subprocess, tempfile, termios, fcntl, threading, time
binary=os.path.abspath('target/debug/toy-agent')
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self,*a): pass
    def do_POST(self):
        data=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        if data['messages'][-1]['role']=='user':
            message={'role':'assistant','content':None,'tool_calls':[{'id':'a1','type':'function','function':{'name':'bash','arguments':json.dumps({'command':'printf tool-ok'})}}]}
        else:
            message={'role':'assistant','content':'# Heading\n\n**bold**\n\n'+'\n\n'.join(f'line-{i:02}' for i in range(60))}
        body=json.dumps({'choices':[{'message':message}]}).encode();self.send_response(200);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
s=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=s.serve_forever,daemon=True).start()
with tempfile.TemporaryDirectory() as tmp:
    config = os.path.join(tmp, 'config.json')
    with open(config, 'w') as f: json.dump({'client': {}}, f)
    env={**os.environ,'TERM':'xterm-256color','TOY_AGENT_CONFIG':config,'TOY_AGENT_SESSIONS':tmp,'OPENAI_API_KEY':'test','OPENAI_MODEL':'test-model','OPENAI_BASE_URL':f'http://127.0.0.1:{s.server_port}/v1'}
    master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',24,90,0,0))
    original_termios = termios.tcgetattr(slave)
    proc=subprocess.Popen([binary],stdin=slave,stdout=slave,stderr=slave,env=env)
    atexit.register(lambda: proc.kill() if proc.poll() is None else None)
    output=b''; offset=0
    def expect(text, timeout=10):
        global output,offset
        needle=text.encode(); deadline=time.monotonic()+timeout
        while True:
            i=output.find(needle,offset)
            if i>=0: offset=i+len(needle); return
            if time.monotonic()>deadline: raise AssertionError(f'missing {text!r}; tail: {output[-2500:]!r}')
            if select.select([master],[],[],.2)[0]:
                try: output+=os.read(master,65536)
                except OSError: raise AssertionError(f'exited {proc.poll()}; tail: {output[-2500:]!r}')
    def send(data): os.write(master,data.encode())
    expect('test-model'); expect('❯ ')
    start = offset
    send('z'); expect('z')
    assert '❯'.encode() not in output[start:offset], 'input prompt was repainted while typing'
    send('\x1b[200~first\n第二\x1b[201~'); expect('第二')
    send('\x03'); send('hello\r'); expect('Allow? [y/N]'); send('y\n'); expect('Heading');expect('bold');expect('line-59');expect('test-model')
    assert output.count(b'\x1b[?1049h')==1, output.count(b'\x1b[?1049h')
    assert b'\x1b[?1049l' not in output, 'left alternate screen during request'
    send('\x1b[5~'); expect('line-')
    send('\x1b[6~'); expect('line-')
    send('!printf shell-ok\r');expect('shell-ok');expect('test-model')
    assert b'\x1b[?1049l' not in output, 'left alternate screen during shell command'
    send('/quit\r');proc.wait(timeout=10)
    while select.select([master],[],[],0)[0]:
        try: output+=os.read(master,65536)
        except OSError: break
    assert proc.returncode==0,proc.returncode
    assert output.count(b'\x1b[?1049l')==1,output.count(b'\x1b[?1049l')
    assert termios.tcgetattr(slave) == original_termios, 'terminal settings not restored'
    os.close(slave); os.close(master)
s.shutdown();print('PASS: one alternate-screen entry/exit, no page switch during model/tool/shell, long Markdown output and interactive approval')
