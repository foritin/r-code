import io

p = 'crates/r-code-runtime/src/remote/listener.rs'
s = io.open(p, encoding='utf8').read()

# Register the connection lease after successful auth + select it in the loop
old = '''    registry.note_seen(&device_id);'''
new = '''    registry.note_seen(&device_id);

    // Connection lease: revoke/capability-narrow can drop this socket
    // immediately; the listener shutdown drops every connection too.
    let connection_stop = Arc::new(Notify::new());
    let lease = ConnectionLease {
        device_id: device_id.clone(),
        stop: connection_stop.clone(),
        connections: connections.clone(),
    };
    if let Ok(mut map) = connections.lock() {
        map.entry(device_id.clone()).or_default().push(connection_stop.clone());
    }'''
assert old in s
s = s.replace(old, new, 1)

# Extend the loop select with the two stop sources
old = '''        tokio::select! {
            message = ws.next() => {
                let Some(Ok(message)) = message else { break };'''
new = '''        tokio::select! {
            message = ws.next() => {
                let Some(Ok(message)) = message else { break };'''
assert old in s

old = '''            _ = heartbeat.tick() => {
                if ws.send(Message::Ping(vec![].into())).await.is_err() {
                    break;
                }
            }
        }
    }
    if let Some((id, _)) = subscription {
        hub.unsubscribe(id);
    }
}'''
new = '''            _ = heartbeat.tick() => {
                if ws.send(Message::Ping(vec![].into())).await.is_err() {
                    break;
                }
            }
            _ = connection_stop.notified() => {
                // Device revoked or capabilities narrowed (R11): drop now.
                let _ = ws.close(None).await;
                break;
            }
            _ = shutdown.notified() => {
                // Listener stopped (setListener(false) / last revoke): every
                // connection drops; device records persist.
                let _ = ws.close(None).await;
                break;
            }
        }
    }
    if let Some((id, _)) = subscription {
        hub.unsubscribe(id);
    }
    drop(lease);
}'''
assert old in s
s = s.replace(old, new, 1)

io.open(p, 'w', encoding='utf8', newline='').write(s)
print('lease + stop sources wired')
