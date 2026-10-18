#!/usr/bin/env python3
"""Turns an EWF-E01 written by ewfacquire into an EWF-L01 (logical evidence
file): the signature becomes LVF and an `ltree` section listing files whose
data lies in the media is inserted before `done`. libewf's ewfinfo and
`ewfexport -f files` read the result.

usage: make-l01.py <in.E01> <out.L01>   (media from make-l01.py --media)
"""
import hashlib
import struct
import sys
import zlib

DIR = 0x02000000
TYPES = ['mid', 'ls', 'be', 'id', 'cr', 'ac', 'wr', 'mo', 'p', 'n', 'opr', 'src', 'sub', 'ha']


def media():
    """4 KiB: a.txt at 0, b.bin (512 bytes) at 0x200."""
    data = bytearray(4096)
    a = b'hello from a.txt\n'
    data[0:len(a)] = a
    data[0x200:0x300] = bytes(range(256))
    data[0x300:0x400] = bytes(reversed(range(256)))
    return bytes(data)


def entry(**v):
    return '\t'.join(str(v.get(t, '')) for t in TYPES)


def tree():
    md5 = hashlib.md5(media()[:17]).hexdigest()
    lines = [
        '5',
        'rec', 'tb\tcl', '4096\t1', '',
        'perm', '0\t1', 'p\tn\ts\tpr\tnta\tnti', '0\t0', '1\t\t\t10\t0\t0', '',
        'srce', '0\t1', 'p\tn\tid', '0\t0', '1\t\t0', '',
        'sub', '0\t1', 'p\tn\tid', '0\t0', '1\t\t0', '',
        'entry', '4\t1', '\t'.join(TYPES),
        '0\t2', entry(mid='0' * 32, ls=0, id=0, p=1, opr=DIR),
        '0\t2', entry(mid='1' * 32, ls=0, id=1, p=1, n='docs', cr=1600000000, opr=DIR),
        '0\t0', entry(mid='2' * 32, ls=17, be='1 0 11', id=2, n='a.txt', cr=1600000001,
                      ac=1600000002, wr=1600000003, mo=1600000004, opr=0, ha=md5),
        '0\t0', entry(mid='4' * 32, ls=0, id=4, n='Ääkköset.txt', wr=1600000006, opr=0),
        '0\t0', entry(mid='3' * 32, ls=512, be='1 200 200', id=3, n='b.bin',
                      wr=1600000005, opr=0),
        '', '',
    ]
    return '\n'.join(lines).encode('utf-16-le')


def a32(x):
    return zlib.adler32(bytes(x)) & 0xFFFFFFFF


def desc(kind, nxt, size):
    d = bytearray(76)
    d[:len(kind)] = kind.encode()
    struct.pack_into('<QQ', d, 16, nxt, size)
    struct.pack_into('<I', d, 72, a32(d[:72]))
    return d


def main():
    if sys.argv[1:] == ['--media']:
        sys.stdout.buffer.write(media())
        return
    src, dst = sys.argv[1], sys.argv[2]
    b = bytearray(open(src, 'rb').read())
    off = 13
    while True:
        kind = b[off:off + 16].rstrip(b'\0').decode()
        nxt, _ = struct.unpack_from('<QQ', b, off + 16)
        if kind in ('done', 'next') or nxt <= off:
            break
        off = nxt
    text = tree()
    hdr = bytearray(48)
    hdr[0:16] = hashlib.md5(text).digest()
    struct.pack_into('<Q', hdr, 16, len(text))
    struct.pack_into('<I', hdr, 24, a32(hdr))
    payload = bytes(hdr) + text
    size = 76 + len(payload)
    out = b[:off] + desc('ltree', off + size, size) + payload + desc('done', off + size, 76)
    out[0:8] = b'LVF\x09\x0d\x0a\xff\x00'
    open(dst, 'wb').write(out)


main()
