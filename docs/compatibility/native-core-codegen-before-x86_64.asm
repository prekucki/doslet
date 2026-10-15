; Original eight-instruction RAM benchmark, not game code.
; Linux x86-64 / Cranelift 0.132.3 opt_level=speed, 3048 bytes including constant pool.
Disassembly of section .data:

0000000000000000 <.data>:
   0:	55                   	push   rbp
   1:	48 89 e5             	mov    rbp,rsp
   4:	48 81 ec 80 00 00 00 	sub    rsp,0x80
   b:	48 89 5c 24 50       	mov    QWORD PTR [rsp+0x50],rbx
  10:	4c 89 64 24 58       	mov    QWORD PTR [rsp+0x58],r12
  15:	4c 89 6c 24 60       	mov    QWORD PTR [rsp+0x60],r13
  1a:	4c 89 74 24 68       	mov    QWORD PTR [rsp+0x68],r14
  1f:	4c 89 7c 24 70       	mov    QWORD PTR [rsp+0x70],r15
  24:	48 89 4c 24 10       	mov    QWORD PTR [rsp+0x10],rcx
  29:	4c 89 44 24 18       	mov    QWORD PTR [rsp+0x18],r8
  2e:	4c 89 4c 24 20       	mov    QWORD PTR [rsp+0x20],r9
  33:	4c 8b 45 10          	mov    r8,QWORD PTR [rbp+0x10]
  37:	4c 89 44 24 28       	mov    QWORD PTR [rsp+0x28],r8
  3c:	44 8b 3f             	mov    r15d,DWORD PTR [rdi]
  3f:	8b 4f 04             	mov    ecx,DWORD PTR [rdi+0x4]
  42:	44 8b 6f 08          	mov    r13d,DWORD PTR [rdi+0x8]
  46:	44 8b 57 0c          	mov    r10d,DWORD PTR [rdi+0xc]
  4a:	8b 47 10             	mov    eax,DWORD PTR [rdi+0x10]
  4d:	48 89 44 24 48       	mov    QWORD PTR [rsp+0x48],rax
  52:	8b 47 14             	mov    eax,DWORD PTR [rdi+0x14]
  55:	48 89 44 24 40       	mov    QWORD PTR [rsp+0x40],rax
  5a:	44 8b 5f 18          	mov    r11d,DWORD PTR [rdi+0x18]
  5e:	44 8b 4f 1c          	mov    r9d,DWORD PTR [rdi+0x1c]
  62:	44 8b 06             	mov    r8d,DWORD PTR [rsi]
  65:	48 89 34 24          	mov    QWORD PTR [rsp],rsi
  69:	8b 32                	mov    esi,DWORD PTR [rdx]
  6b:	48 89 54 24 08       	mov    QWORD PTR [rsp+0x8],rdx
  70:	48 89 74 24 38       	mov    QWORD PTR [rsp+0x38],rsi
  75:	4c 8b 64 24 10       	mov    r12,QWORD PTR [rsp+0x10]
  7a:	45 85 e4             	test   r12d,r12d
  7d:	4c 89 64 24 30       	mov    QWORD PTR [rsp+0x30],r12
  82:	0f 85 6f 00 00 00    	jne    0xf7
  88:	44 89 3f             	mov    DWORD PTR [rdi],r15d
  8b:	89 4f 04             	mov    DWORD PTR [rdi+0x4],ecx
  8e:	44 89 6f 08          	mov    DWORD PTR [rdi+0x8],r13d
  92:	44 89 57 0c          	mov    DWORD PTR [rdi+0xc],r10d
  96:	48 8b 4c 24 48       	mov    rcx,QWORD PTR [rsp+0x48]
  9b:	89 4f 10             	mov    DWORD PTR [rdi+0x10],ecx
  9e:	4c 8b 74 24 40       	mov    r14,QWORD PTR [rsp+0x40]
  a3:	44 89 77 14          	mov    DWORD PTR [rdi+0x14],r14d
  a7:	44 89 5f 18          	mov    DWORD PTR [rdi+0x18],r11d
  ab:	44 89 4f 1c          	mov    DWORD PTR [rdi+0x1c],r9d
  af:	48 8b 04 24          	mov    rax,QWORD PTR [rsp]
  b3:	44 89 00             	mov    DWORD PTR [rax],r8d
  b6:	48 8b 5c 24 08       	mov    rbx,QWORD PTR [rsp+0x8]
  bb:	48 8b 74 24 38       	mov    rsi,QWORD PTR [rsp+0x38]
  c0:	89 33                	mov    DWORD PTR [rbx],esi
  c2:	48 8b 54 24 10       	mov    rdx,QWORD PTR [rsp+0x10]
  c7:	4c 8b 64 24 30       	mov    r12,QWORD PTR [rsp+0x30]
  cc:	48 89 d0             	mov    rax,rdx
  cf:	41 2b c4             	sub    eax,r12d
  d2:	48 8b 5c 24 50       	mov    rbx,QWORD PTR [rsp+0x50]
  d7:	4c 8b 64 24 58       	mov    r12,QWORD PTR [rsp+0x58]
  dc:	4c 8b 6c 24 60       	mov    r13,QWORD PTR [rsp+0x60]
  e1:	4c 8b 74 24 68       	mov    r14,QWORD PTR [rsp+0x68]
  e6:	4c 8b 7c 24 70       	mov    r15,QWORD PTR [rsp+0x70]
  eb:	48 81 c4 80 00 00 00 	add    rsp,0x80
  f2:	48 89 ec             	mov    rsp,rbp
  f5:	5d                   	pop    rbp
  f6:	c3                   	ret
  f7:	41 8b f3             	mov    esi,r11d
  fa:	48 8b 44 24 18       	mov    rax,QWORD PTR [rsp+0x18]
  ff:	8b d0                	mov    edx,eax
 101:	48 3b f2             	cmp    rsi,rdx
 104:	0f 93 c0             	setae  al
 107:	48 89 f3             	mov    rbx,rsi
 10a:	48 2b da             	sub    rbx,rdx
 10d:	4c 8d 73 04          	lea    r14,[rbx+0x4]
 111:	4c 8b 64 24 28       	mov    r12,QWORD PTR [rsp+0x28]
 116:	4d 3b f4             	cmp    r14,r12
 119:	41 0f 96 c4          	setbe  r12b
 11d:	48 3b 35 b4 0a 00 00 	cmp    rsi,QWORD PTR [rip+0xab4]        # 0xbd8
 124:	40 0f 96 c6          	setbe  sil
 128:	44 23 e6             	and    r12d,esi
 12b:	41 84 c4             	test   r12b,al
 12e:	0f 85 6f 00 00 00    	jne    0x1a3
 134:	44 89 3f             	mov    DWORD PTR [rdi],r15d
 137:	89 4f 04             	mov    DWORD PTR [rdi+0x4],ecx
 13a:	44 89 6f 08          	mov    DWORD PTR [rdi+0x8],r13d
 13e:	44 89 57 0c          	mov    DWORD PTR [rdi+0xc],r10d
 142:	48 8b 4c 24 48       	mov    rcx,QWORD PTR [rsp+0x48]
 147:	89 4f 10             	mov    DWORD PTR [rdi+0x10],ecx
 14a:	4c 8b 74 24 40       	mov    r14,QWORD PTR [rsp+0x40]
 14f:	44 89 77 14          	mov    DWORD PTR [rdi+0x14],r14d
 153:	44 89 5f 18          	mov    DWORD PTR [rdi+0x18],r11d
 157:	44 89 4f 1c          	mov    DWORD PTR [rdi+0x1c],r9d
 15b:	48 8b 04 24          	mov    rax,QWORD PTR [rsp]
 15f:	44 89 00             	mov    DWORD PTR [rax],r8d
 162:	48 8b 5c 24 08       	mov    rbx,QWORD PTR [rsp+0x8]
 167:	48 8b 74 24 38       	mov    rsi,QWORD PTR [rsp+0x38]
 16c:	89 33                	mov    DWORD PTR [rbx],esi
 16e:	48 8b 54 24 10       	mov    rdx,QWORD PTR [rsp+0x10]
 173:	4c 8b 64 24 30       	mov    r12,QWORD PTR [rsp+0x30]
 178:	48 89 d0             	mov    rax,rdx
 17b:	41 2b c4             	sub    eax,r12d
 17e:	48 8b 5c 24 50       	mov    rbx,QWORD PTR [rsp+0x50]
 183:	4c 8b 64 24 58       	mov    r12,QWORD PTR [rsp+0x58]
 188:	4c 8b 6c 24 60       	mov    r13,QWORD PTR [rsp+0x60]
 18d:	4c 8b 74 24 68       	mov    r14,QWORD PTR [rsp+0x68]
 192:	4c 8b 7c 24 70       	mov    r15,QWORD PTR [rsp+0x70]
 197:	48 81 c4 80 00 00 00 	add    rsp,0x80
 19e:	48 89 ec             	mov    rsp,rbp
 1a1:	5d                   	pop    rbp
 1a2:	c3                   	ret
 1a3:	48 8b 44 24 20       	mov    rax,QWORD PTR [rsp+0x20]
 1a8:	44 8b 3c 18          	mov    r15d,DWORD PTR [rax+rbx*1]
 1ac:	4c 8b 64 24 30       	mov    r12,QWORD PTR [rsp+0x30]
 1b1:	41 83 fc 01          	cmp    r12d,0x1
 1b5:	0f 85 77 00 00 00    	jne    0x232
 1bb:	44 89 3f             	mov    DWORD PTR [rdi],r15d
 1be:	89 4f 04             	mov    DWORD PTR [rdi+0x4],ecx
 1c1:	44 89 6f 08          	mov    DWORD PTR [rdi+0x8],r13d
 1c5:	44 89 57 0c          	mov    DWORD PTR [rdi+0xc],r10d
 1c9:	48 8b 4c 24 48       	mov    rcx,QWORD PTR [rsp+0x48]
 1ce:	89 4f 10             	mov    DWORD PTR [rdi+0x10],ecx
 1d1:	4c 8b 74 24 40       	mov    r14,QWORD PTR [rsp+0x40]
 1d6:	44 89 77 14          	mov    DWORD PTR [rdi+0x14],r14d
 1da:	44 89 5f 18          	mov    DWORD PTR [rdi+0x18],r11d
 1de:	44 89 4f 1c          	mov    DWORD PTR [rdi+0x1c],r9d
 1e2:	48 8b 04 24          	mov    rax,QWORD PTR [rsp]
 1e6:	44 89 00             	mov    DWORD PTR [rax],r8d
 1e9:	48 8b 74 24 38       	mov    rsi,QWORD PTR [rsp+0x38]
 1ee:	8d 7e 02             	lea    edi,[rsi+0x2]
 1f1:	48 8b 5c 24 08       	mov    rbx,QWORD PTR [rsp+0x8]
 1f6:	89 3b                	mov    DWORD PTR [rbx],edi
 1f8:	4c 8b 64 24 30       	mov    r12,QWORD PTR [rsp+0x30]
 1fd:	45 8d 44 24 ff       	lea    r8d,[r12-0x1]
 202:	48 8b 54 24 10       	mov    rdx,QWORD PTR [rsp+0x10]
 207:	48 89 d0             	mov    rax,rdx
 20a:	41 2b c0             	sub    eax,r8d
 20d:	48 8b 5c 24 50       	mov    rbx,QWORD PTR [rsp+0x50]
 212:	4c 8b 64 24 58       	mov    r12,QWORD PTR [rsp+0x58]
 217:	4c 8b 6c 24 60       	mov    r13,QWORD PTR [rsp+0x60]
 21c:	4c 8b 74 24 68       	mov    r14,QWORD PTR [rsp+0x68]
 221:	4c 8b 7c 24 70       	mov    r15,QWORD PTR [rsp+0x70]
 226:	48 81 c4 80 00 00 00 	add    rsp,0x80
 22d:	48 89 ec             	mov    rsp,rbp
 230:	5d                   	pop    rbp
 231:	c3                   	ret
 232:	4c 8b 64 24 30       	mov    r12,QWORD PTR [rsp+0x30]
 237:	41 83 fc 02          	cmp    r12d,0x2
 23b:	0f 85 f0 00 00 00    	jne    0x331
 241:	44 89 3f             	mov    DWORD PTR [rdi],r15d
 244:	89 4f 04             	mov    DWORD PTR [rdi+0x4],ecx
 247:	44 89 6f 08          	mov    DWORD PTR [rdi+0x8],r13d
 24b:	44 89 57 0c          	mov    DWORD PTR [rdi+0xc],r10d
 24f:	48 8b 4c 24 48       	mov    rcx,QWORD PTR [rsp+0x48]
 254:	89 4f 10             	mov    DWORD PTR [rdi+0x10],ecx
 257:	4c 8b 74 24 40       	mov    r14,QWORD PTR [rsp+0x40]
 25c:	44 89 77 14          	mov    DWORD PTR [rdi+0x14],r14d
 260:	44 89 5f 18          	mov    DWORD PTR [rdi+0x18],r11d
 264:	44 89 4f 1c          	mov    DWORD PTR [rdi+0x1c],r9d
 268:	41 81 e0 2a f7 ff ff 	and    r8d,0xfffff72a
 26f:	41 83 c8 02          	or     r8d,0x2
 273:	4c 89 f9             	mov    rcx,r15
 276:	41 2b cd             	sub    ecx,r13d
 279:	85 c9                	test   ecx,ecx
 27b:	0f 94 c0             	sete   al
 27e:	0f b6 c0             	movzx  eax,al
 281:	c1 e0 06             	shl    eax,0x6
 284:	44 0b c0             	or     r8d,eax
 287:	48 89 c8             	mov    rax,rcx
 28a:	c1 e8 18             	shr    eax,0x18
 28d:	25 80 00 00 00       	and    eax,0x80
 292:	44 0b c0             	or     r8d,eax
 295:	48 89 c8             	mov    rax,rcx
 298:	25 ff 00 00 00       	and    eax,0xff
 29d:	f3 0f b8 c0          	popcnt eax,eax
 2a1:	83 e0 01             	and    eax,0x1
 2a4:	83 f0 01             	xor    eax,0x1
 2a7:	c1 e0 02             	shl    eax,0x2
 2aa:	44 0b c0             	or     r8d,eax
 2ad:	41 8b c7             	mov    eax,r15d
 2b0:	41 8b d5             	mov    edx,r13d
 2b3:	48 3b c2             	cmp    rax,rdx
 2b6:	0f 92 c0             	setb   al
 2b9:	0f b6 c0             	movzx  eax,al
 2bc:	44 0b c0             	or     r8d,eax
 2bf:	4c 89 fa             	mov    rdx,r15
 2c2:	41 33 d5             	xor    edx,r13d
 2c5:	48 89 d0             	mov    rax,rdx
 2c8:	33 c1                	xor    eax,ecx
 2ca:	83 e0 10             	and    eax,0x10
 2cd:	44 0b c0             	or     r8d,eax
 2d0:	44 33 f9             	xor    r15d,ecx
 2d3:	41 23 d7             	and    edx,r15d
 2d6:	c1 ea 14             	shr    edx,0x14
 2d9:	81 e2 00 08 00 00    	and    edx,0x800
 2df:	44 0b c2             	or     r8d,edx
 2e2:	48 8b 04 24          	mov    rax,QWORD PTR [rsp]
 2e6:	44 89 00             	mov    DWORD PTR [rax],r8d
 2e9:	48 8b 74 24 38       	mov    rsi,QWORD PTR [rsp+0x38]
 2ee:	8d 46 04             	lea    eax,[rsi+0x4]
 2f1:	48 8b 5c 24 08       	mov    rbx,QWORD PTR [rsp+0x8]
 2f6:	89 03                	mov    DWORD PTR [rbx],eax
 2f8:	4c 8b 64 24 30       	mov    r12,QWORD PTR [rsp+0x30]
 2fd:	41 8d 4c 24 fe       	lea    ecx,[r12-0x2]
 302:	48 8b 54 24 10       	mov    rdx,QWORD PTR [rsp+0x10]
 307:	48 89 d0             	mov    rax,rdx
 30a:	2b c1                	sub    eax,ecx
 30c:	48 8b 5c 24 50       	mov    rbx,QWORD PTR [rsp+0x50]
 311:	4c 8b 64 24 58       	mov    r12,QWORD PTR [rsp+0x58]
 316:	4c 8b 6c 24 60       	mov    r13,QWORD PTR [rsp+0x60]
 31b:	4c 8b 74 24 68       	mov    r14,QWORD PTR [rsp+0x68]
 320:	4c 8b 7c 24 70       	mov    r15,QWORD PTR [rsp+0x70]
 325:	48 81 c4 80 00 00 00 	add    rsp,0x80
 32c:	48 89 ec             	mov    rsp,rbp
 32f:	5d                   	pop    rbp
 330:	c3                   	ret
 331:	41 81 e0 2a f7 ff ff 	and    r8d,0xfffff72a
 338:	41 83 c8 02          	or     r8d,0x2
 33c:	4c 89 fb             	mov    rbx,r15
 33f:	41 2b dd             	sub    ebx,r13d
 342:	85 db                	test   ebx,ebx
 344:	0f 94 c0             	sete   al
 347:	0f b6 c0             	movzx  eax,al
 34a:	c1 e0 06             	shl    eax,0x6
 34d:	44 0b c0             	or     r8d,eax
 350:	48 89 d8             	mov    rax,rbx
 353:	c1 e8 18             	shr    eax,0x18
 356:	25 80 00 00 00       	and    eax,0x80
 35b:	44 0b c0             	or     r8d,eax
 35e:	48 89 d8             	mov    rax,rbx
 361:	25 ff 00 00 00       	and    eax,0xff
 366:	f3 0f b8 c0          	popcnt eax,eax
 36a:	83 e0 01             	and    eax,0x1
 36d:	83 f0 01             	xor    eax,0x1
 370:	c1 e0 02             	shl    eax,0x2
 373:	44 0b c0             	or     r8d,eax
 376:	41 8b c7             	mov    eax,r15d
 379:	41 8b f5             	mov    esi,r13d
 37c:	48 3b c6             	cmp    rax,rsi
 37f:	0f 92 c0             	setb   al
 382:	0f b6 c0             	movzx  eax,al
 385:	44 0b c0             	or     r8d,eax
 388:	4c 89 fe             	mov    rsi,r15
 38b:	41 33 f5             	xor    esi,r13d
 38e:	48 89 f0             	mov    rax,rsi
 391:	33 c3                	xor    eax,ebx
 393:	83 e0 10             	and    eax,0x10
 396:	44 0b c0             	or     r8d,eax
 399:	4c 89 f8             	mov    rax,r15
 39c:	33 c3                	xor    eax,ebx
 39e:	23 f0                	and    esi,eax
 3a0:	c1 ee 14             	shr    esi,0x14
 3a3:	81 e6 00 08 00 00    	and    esi,0x800
 3a9:	44 0b c6             	or     r8d,esi
 3ac:	41 f7 c0 40 00 00 00 	test   r8d,0x40
 3b3:	0f 85 a7 07 00 00    	jne    0xb60
 3b9:	4c 8b 64 24 30       	mov    r12,QWORD PTR [rsp+0x30]
 3be:	41 83 fc 03          	cmp    r12d,0x3
 3c2:	0f 85 76 00 00 00    	jne    0x43e
 3c8:	44 89 3f             	mov    DWORD PTR [rdi],r15d
 3cb:	89 4f 04             	mov    DWORD PTR [rdi+0x4],ecx
 3ce:	44 89 6f 08          	mov    DWORD PTR [rdi+0x8],r13d
 3d2:	44 89 57 0c          	mov    DWORD PTR [rdi+0xc],r10d
 3d6:	48 8b 4c 24 48       	mov    rcx,QWORD PTR [rsp+0x48]
 3db:	89 4f 10             	mov    DWORD PTR [rdi+0x10],ecx
 3de:	4c 8b 74 24 40       	mov    r14,QWORD PTR [rsp+0x40]
 3e3:	44 89 77 14          	mov    DWORD PTR [rdi+0x14],r14d
 3e7:	44 89 5f 18          	mov    DWORD PTR [rdi+0x18],r11d
 3eb:	44 89 4f 1c          	mov    DWORD PTR [rdi+0x1c],r9d
 3ef:	48 8b 04 24          	mov    rax,QWORD PTR [rsp]
 3f3:	44 89 00             	mov    DWORD PTR [rax],r8d
 3f6:	48 8b 74 24 38       	mov    rsi,QWORD PTR [rsp+0x38]
 3fb:	8d 46 06             	lea    eax,[rsi+0x6]
 3fe:	48 8b 5c 24 08       	mov    rbx,QWORD PTR [rsp+0x8]
 403:	89 03                	mov    DWORD PTR [rbx],eax
 405:	4c 8b 64 24 30       	mov    r12,QWORD PTR [rsp+0x30]
 40a:	41 8d 4c 24 fd       	lea    ecx,[r12-0x3]
 40f:	48 8b 54 24 10       	mov    rdx,QWORD PTR [rsp+0x10]
 414:	48 89 d0             	mov    rax,rdx
 417:	2b c1                	sub    eax,ecx
 419:	48 8b 5c 24 50       	mov    rbx,QWORD PTR [rsp+0x50]
 41e:	4c 8b 64 24 58       	mov    r12,QWORD PTR [rsp+0x58]
 423:	4c 8b 6c 24 60       	mov    r13,QWORD PTR [rsp+0x60]
 428:	4c 8b 74 24 68       	mov    r14,QWORD PTR [rsp+0x68]
 42d:	4c 8b 7c 24 70       	mov    r15,QWORD PTR [rsp+0x70]
 432:	48 81 c4 80 00 00 00 	add    rsp,0x80
 439:	48 89 ec             	mov    rsp,rbp
 43c:	5d                   	pop    rbp
 43d:	c3                   	ret
 43e:	41 8d 43 04          	lea    eax,[r11+0x4]
 442:	48 3b c2             	cmp    rax,rdx
 445:	0f 93 c3             	setae  bl
 448:	48 89 c6             	mov    rsi,rax
 44b:	48 2b f2             	sub    rsi,rdx
 44e:	4c 8d 66 04          	lea    r12,[rsi+0x4]
 452:	4c 8b 74 24 28       	mov    r14,QWORD PTR [rsp+0x28]
 457:	4d 3b e6             	cmp    r12,r14
 45a:	41 0f 96 c4          	setbe  r12b
 45e:	48 3b 05 73 07 00 00 	cmp    rax,QWORD PTR [rip+0x773]        # 0xbd8
 465:	0f 96 c0             	setbe  al
 468:	44 23 e0             	and    r12d,eax
 46b:	41 84 dc             	test   r12b,bl
 46e:	0f 85 76 00 00 00    	jne    0x4ea
 474:	44 89 3f             	mov    DWORD PTR [rdi],r15d
 477:	89 4f 04             	mov    DWORD PTR [rdi+0x4],ecx
 47a:	44 89 6f 08          	mov    DWORD PTR [rdi+0x8],r13d
 47e:	44 89 57 0c          	mov    DWORD PTR [rdi+0xc],r10d
 482:	48 8b 4c 24 48       	mov    rcx,QWORD PTR [rsp+0x48]
 487:	89 4f 10             	mov    DWORD PTR [rdi+0x10],ecx
 48a:	4c 8b 74 24 40       	mov    r14,QWORD PTR [rsp+0x40]
 48f:	44 89 77 14          	mov    DWORD PTR [rdi+0x14],r14d
 493:	44 89 5f 18          	mov    DWORD PTR [rdi+0x18],r11d
 497:	44 89 4f 1c          	mov    DWORD PTR [rdi+0x1c],r9d
 49b:	48 8b 04 24          	mov    rax,QWORD PTR [rsp]
 49f:	44 89 00             	mov    DWORD PTR [rax],r8d
 4a2:	48 8b 74 24 38       	mov    rsi,QWORD PTR [rsp+0x38]
 4a7:	8d 46 06             	lea    eax,[rsi+0x6]
 4aa:	48 8b 5c 24 08       	mov    rbx,QWORD PTR [rsp+0x8]
 4af:	89 03                	mov    DWORD PTR [rbx],eax
 4b1:	4c 8b 64 24 30       	mov    r12,QWORD PTR [rsp+0x30]
 4b6:	41 8d 4c 24 fd       	lea    ecx,[r12-0x3]
 4bb:	48 8b 54 24 10       	mov    rdx,QWORD PTR [rsp+0x10]
 4c0:	48 89 d0             	mov    rax,rdx
 4c3:	2b c1                	sub    eax,ecx
 4c5:	48 8b 5c 24 50       	mov    rbx,QWORD PTR [rsp+0x50]
 4ca:	4c 8b 64 24 58       	mov    r12,QWORD PTR [rsp+0x58]
 4cf:	4c 8b 6c 24 60       	mov    r13,QWORD PTR [rsp+0x60]
 4d4:	4c 8b 74 24 68       	mov    r14,QWORD PTR [rsp+0x68]
 4d9:	4c 8b 7c 24 70       	mov    r15,QWORD PTR [rsp+0x70]
 4de:	48 81 c4 80 00 00 00 	add    rsp,0x80
 4e5:	48 89 ec             	mov    rsp,rbp
 4e8:	5d                   	pop    rbp
 4e9:	c3                   	ret
 4ea:	48 8b 44 24 20       	mov    rax,QWORD PTR [rsp+0x20]
 4ef:	44 8b 14 30          	mov    r10d,DWORD PTR [rax+rsi*1]
 4f3:	4c 8b 64 24 30       	mov    r12,QWORD PTR [rsp+0x30]
 4f8:	41 83 fc 04          	cmp    r12d,0x4
 4fc:	0f 85 79 00 00 00    	jne    0x57b
 502:	44 89 3f             	mov    DWORD PTR [rdi],r15d
 505:	89 4f 04             	mov    DWORD PTR [rdi+0x4],ecx
 508:	44 89 6f 08          	mov    DWORD PTR [rdi+0x8],r13d
 50c:	44 89 57 0c          	mov    DWORD PTR [rdi+0xc],r10d
 510:	48 8b 4c 24 48       	mov    rcx,QWORD PTR [rsp+0x48]
 515:	89 4f 10             	mov    DWORD PTR [rdi+0x10],ecx
 518:	4c 8b 74 24 40       	mov    r14,QWORD PTR [rsp+0x40]
 51d:	44 89 77 14          	mov    DWORD PTR [rdi+0x14],r14d
 521:	44 89 5f 18          	mov    DWORD PTR [rdi+0x18],r11d
 525:	44 89 4f 1c          	mov    DWORD PTR [rdi+0x1c],r9d
 529:	48 8b 04 24          	mov    rax,QWORD PTR [rsp]
 52d:	44 89 00             	mov    DWORD PTR [rax],r8d
 530:	48 8b 74 24 38       	mov    rsi,QWORD PTR [rsp+0x38]
 535:	44 8d 46 09          	lea    r8d,[rsi+0x9]
 539:	48 8b 5c 24 08       	mov    rbx,QWORD PTR [rsp+0x8]
 53e:	44 89 03             	mov    DWORD PTR [rbx],r8d
 541:	4c 8b 64 24 30       	mov    r12,QWORD PTR [rsp+0x30]
 546:	45 8d 4c 24 fc       	lea    r9d,[r12-0x4]
 54b:	48 8b 54 24 10       	mov    rdx,QWORD PTR [rsp+0x10]
 550:	48 89 d0             	mov    rax,rdx
 553:	41 2b c1             	sub    eax,r9d
 556:	48 8b 5c 24 50       	mov    rbx,QWORD PTR [rsp+0x50]
 55b:	4c 8b 64 24 58       	mov    r12,QWORD PTR [rsp+0x58]
 560:	4c 8b 6c 24 60       	mov    r13,QWORD PTR [rsp+0x60]
 565:	4c 8b 74 24 68       	mov    r14,QWORD PTR [rsp+0x68]
 56a:	4c 8b 7c 24 70       	mov    r15,QWORD PTR [rsp+0x70]
 56f:	48 81 c4 80 00 00 00 	add    rsp,0x80
 576:	48 89 ec             	mov    rsp,rbp
 579:	5d                   	pop    rbp
 57a:	c3                   	ret
 57b:	4c 8b 64 24 30       	mov    r12,QWORD PTR [rsp+0x30]
 580:	41 83 fc 05          	cmp    r12d,0x5
 584:	0f 85 f5 00 00 00    	jne    0x67f
 58a:	44 89 3f             	mov    DWORD PTR [rdi],r15d
 58d:	89 4f 04             	mov    DWORD PTR [rdi+0x4],ecx
 590:	44 89 6f 08          	mov    DWORD PTR [rdi+0x8],r13d
 594:	44 89 57 0c          	mov    DWORD PTR [rdi+0xc],r10d
 598:	48 8b 4c 24 48       	mov    rcx,QWORD PTR [rsp+0x48]
 59d:	89 4f 10             	mov    DWORD PTR [rdi+0x10],ecx
 5a0:	4c 8b 74 24 40       	mov    r14,QWORD PTR [rsp+0x40]
 5a5:	44 89 77 14          	mov    DWORD PTR [rdi+0x14],r14d
 5a9:	44 89 5f 18          	mov    DWORD PTR [rdi+0x18],r11d
 5ad:	43 8d 0c 11          	lea    ecx,[r9+r10*1]
 5b1:	89 4f 1c             	mov    DWORD PTR [rdi+0x1c],ecx
 5b4:	41 81 e0 2a f7 ff ff 	and    r8d,0xfffff72a
 5bb:	41 83 c8 02          	or     r8d,0x2
 5bf:	85 c9                	test   ecx,ecx
 5c1:	0f 94 c0             	sete   al
 5c4:	0f b6 c0             	movzx  eax,al
 5c7:	c1 e0 06             	shl    eax,0x6
 5ca:	44 0b c0             	or     r8d,eax
 5cd:	48 89 c8             	mov    rax,rcx
 5d0:	c1 e8 18             	shr    eax,0x18
 5d3:	25 80 00 00 00       	and    eax,0x80
 5d8:	44 0b c0             	or     r8d,eax
 5db:	48 89 c8             	mov    rax,rcx
 5de:	25 ff 00 00 00       	and    eax,0xff
 5e3:	f3 0f b8 c0          	popcnt eax,eax
 5e7:	83 e0 01             	and    eax,0x1
 5ea:	83 f0 01             	xor    eax,0x1
 5ed:	c1 e0 02             	shl    eax,0x2
 5f0:	44 0b c0             	or     r8d,eax
 5f3:	41 8b c1             	mov    eax,r9d
 5f6:	41 8b d2             	mov    edx,r10d
 5f9:	48 03 c2             	add    rax,rdx
 5fc:	48 3b 05 dd 05 00 00 	cmp    rax,QWORD PTR [rip+0x5dd]        # 0xbe0
 603:	0f 97 c0             	seta   al
 606:	0f b6 c0             	movzx  eax,al
 609:	44 0b c0             	or     r8d,eax
 60c:	4c 89 c8             	mov    rax,r9
 60f:	41 33 c2             	xor    eax,r10d
 612:	48 89 c2             	mov    rdx,rax
 615:	33 d1                	xor    edx,ecx
 617:	83 e2 10             	and    edx,0x10
 61a:	44 0b c2             	or     r8d,edx
 61d:	44 33 c9             	xor    r9d,ecx
 620:	c4 c2 78 f2 c1       	andn   eax,eax,r9d
 625:	c1 e8 14             	shr    eax,0x14
 628:	25 00 08 00 00       	and    eax,0x800
 62d:	44 0b c0             	or     r8d,eax
 630:	48 8b 04 24          	mov    rax,QWORD PTR [rsp]
 634:	44 89 00             	mov    DWORD PTR [rax],r8d
 637:	48 8b 74 24 38       	mov    rsi,QWORD PTR [rsp+0x38]
 63c:	8d 46 0b             	lea    eax,[rsi+0xb]
 63f:	48 8b 5c 24 08       	mov    rbx,QWORD PTR [rsp+0x8]
 644:	89 03                	mov    DWORD PTR [rbx],eax
 646:	4c 8b 64 24 30       	mov    r12,QWORD PTR [rsp+0x30]
 64b:	41 8d 4c 24 fb       	lea    ecx,[r12-0x5]
 650:	48 8b 54 24 10       	mov    rdx,QWORD PTR [rsp+0x10]
 655:	48 89 d0             	mov    rax,rdx
 658:	2b c1                	sub    eax,ecx
 65a:	48 8b 5c 24 50       	mov    rbx,QWORD PTR [rsp+0x50]
 65f:	4c 8b 64 24 58       	mov    r12,QWORD PTR [rsp+0x58]
 664:	4c 8b 6c 24 60       	mov    r13,QWORD PTR [rsp+0x60]
 669:	4c 8b 74 24 68       	mov    r14,QWORD PTR [rsp+0x68]
 66e:	4c 8b 7c 24 70       	mov    r15,QWORD PTR [rsp+0x70]
 673:	48 81 c4 80 00 00 00 	add    rsp,0x80
 67a:	48 89 ec             	mov    rsp,rbp
 67d:	5d                   	pop    rbp
 67e:	c3                   	ret
 67f:	41 8d 43 08          	lea    eax,[r11+0x8]
 683:	48 3b c2             	cmp    rax,rdx
 686:	0f 93 c3             	setae  bl
 689:	48 89 c6             	mov    rsi,rax
 68c:	48 2b f2             	sub    rsi,rdx
 68f:	48 8d 56 04          	lea    rdx,[rsi+0x4]
 693:	4c 8b 64 24 28       	mov    r12,QWORD PTR [rsp+0x28]
 698:	49 3b d4             	cmp    rdx,r12
 69b:	0f 96 c2             	setbe  dl
 69e:	48 3b 05 33 05 00 00 	cmp    rax,QWORD PTR [rip+0x533]        # 0xbd8
 6a5:	0f 96 c0             	setbe  al
 6a8:	23 d0                	and    edx,eax
 6aa:	84 da                	test   dl,bl
 6ac:	0f 85 f5 00 00 00    	jne    0x7a7
 6b2:	44 89 3f             	mov    DWORD PTR [rdi],r15d
 6b5:	89 4f 04             	mov    DWORD PTR [rdi+0x4],ecx
 6b8:	44 89 6f 08          	mov    DWORD PTR [rdi+0x8],r13d
 6bc:	44 89 57 0c          	mov    DWORD PTR [rdi+0xc],r10d
 6c0:	48 8b 4c 24 48       	mov    rcx,QWORD PTR [rsp+0x48]
 6c5:	89 4f 10             	mov    DWORD PTR [rdi+0x10],ecx
 6c8:	4c 8b 74 24 40       	mov    r14,QWORD PTR [rsp+0x40]
 6cd:	44 89 77 14          	mov    DWORD PTR [rdi+0x14],r14d
 6d1:	44 89 5f 18          	mov    DWORD PTR [rdi+0x18],r11d
 6d5:	43 8d 0c 11          	lea    ecx,[r9+r10*1]
 6d9:	89 4f 1c             	mov    DWORD PTR [rdi+0x1c],ecx
 6dc:	41 81 e0 2a f7 ff ff 	and    r8d,0xfffff72a
 6e3:	41 83 c8 02          	or     r8d,0x2
 6e7:	85 c9                	test   ecx,ecx
 6e9:	0f 94 c0             	sete   al
 6ec:	0f b6 c0             	movzx  eax,al
 6ef:	c1 e0 06             	shl    eax,0x6
 6f2:	44 0b c0             	or     r8d,eax
 6f5:	48 89 c8             	mov    rax,rcx
 6f8:	c1 e8 18             	shr    eax,0x18
 6fb:	25 80 00 00 00       	and    eax,0x80
 700:	44 0b c0             	or     r8d,eax
 703:	48 89 c8             	mov    rax,rcx
 706:	25 ff 00 00 00       	and    eax,0xff
 70b:	f3 0f b8 c0          	popcnt eax,eax
 70f:	83 e0 01             	and    eax,0x1
 712:	83 f0 01             	xor    eax,0x1
 715:	c1 e0 02             	shl    eax,0x2
 718:	44 0b c0             	or     r8d,eax
 71b:	41 8b c1             	mov    eax,r9d
 71e:	41 8b d2             	mov    edx,r10d
 721:	48 03 c2             	add    rax,rdx
 724:	48 3b 05 b5 04 00 00 	cmp    rax,QWORD PTR [rip+0x4b5]        # 0xbe0
 72b:	0f 97 c0             	seta   al
 72e:	0f b6 c0             	movzx  eax,al
 731:	44 0b c0             	or     r8d,eax
 734:	4c 89 c8             	mov    rax,r9
 737:	41 33 c2             	xor    eax,r10d
 73a:	48 89 c2             	mov    rdx,rax
 73d:	33 d1                	xor    edx,ecx
 73f:	83 e2 10             	and    edx,0x10
 742:	44 0b c2             	or     r8d,edx
 745:	44 33 c9             	xor    r9d,ecx
 748:	c4 c2 78 f2 c1       	andn   eax,eax,r9d
 74d:	c1 e8 14             	shr    eax,0x14
 750:	25 00 08 00 00       	and    eax,0x800
 755:	44 0b c0             	or     r8d,eax
 758:	48 8b 04 24          	mov    rax,QWORD PTR [rsp]
 75c:	44 89 00             	mov    DWORD PTR [rax],r8d
 75f:	48 8b 74 24 38       	mov    rsi,QWORD PTR [rsp+0x38]
 764:	8d 46 0b             	lea    eax,[rsi+0xb]
 767:	48 8b 5c 24 08       	mov    rbx,QWORD PTR [rsp+0x8]
 76c:	89 03                	mov    DWORD PTR [rbx],eax
 76e:	4c 8b 64 24 30       	mov    r12,QWORD PTR [rsp+0x30]
 773:	41 8d 4c 24 fb       	lea    ecx,[r12-0x5]
 778:	48 8b 54 24 10       	mov    rdx,QWORD PTR [rsp+0x10]
 77d:	48 89 d0             	mov    rax,rdx
 780:	2b c1                	sub    eax,ecx
 782:	48 8b 5c 24 50       	mov    rbx,QWORD PTR [rsp+0x50]
 787:	4c 8b 64 24 58       	mov    r12,QWORD PTR [rsp+0x58]
 78c:	4c 8b 6c 24 60       	mov    r13,QWORD PTR [rsp+0x60]
 791:	4c 8b 74 24 68       	mov    r14,QWORD PTR [rsp+0x68]
 796:	4c 8b 7c 24 70       	mov    r15,QWORD PTR [rsp+0x70]
 79b:	48 81 c4 80 00 00 00 	add    rsp,0x80
 7a2:	48 89 ec             	mov    rsp,rbp
 7a5:	5d                   	pop    rbp
 7a6:	c3                   	ret
 7a7:	48 8b 54 24 20       	mov    rdx,QWORD PTR [rsp+0x20]
 7ac:	44 8b 1c 32          	mov    r11d,DWORD PTR [rdx+rsi*1]
 7b0:	4c 8b 64 24 30       	mov    r12,QWORD PTR [rsp+0x30]
 7b5:	41 83 fc 06          	cmp    r12d,0x6
 7b9:	0f 85 f5 00 00 00    	jne    0x8b4
 7bf:	44 89 3f             	mov    DWORD PTR [rdi],r15d
 7c2:	89 4f 04             	mov    DWORD PTR [rdi+0x4],ecx
 7c5:	44 89 6f 08          	mov    DWORD PTR [rdi+0x8],r13d
 7c9:	44 89 57 0c          	mov    DWORD PTR [rdi+0xc],r10d
 7cd:	48 8b 4c 24 48       	mov    rcx,QWORD PTR [rsp+0x48]
 7d2:	89 4f 10             	mov    DWORD PTR [rdi+0x10],ecx
 7d5:	4c 8b 74 24 40       	mov    r14,QWORD PTR [rsp+0x40]
 7da:	44 89 77 14          	mov    DWORD PTR [rdi+0x14],r14d
 7de:	44 89 5f 18          	mov    DWORD PTR [rdi+0x18],r11d
 7e2:	43 8d 0c 11          	lea    ecx,[r9+r10*1]
 7e6:	89 4f 1c             	mov    DWORD PTR [rdi+0x1c],ecx
 7e9:	41 81 e0 2a f7 ff ff 	and    r8d,0xfffff72a
 7f0:	41 83 c8 02          	or     r8d,0x2
 7f4:	85 c9                	test   ecx,ecx
 7f6:	0f 94 c0             	sete   al
 7f9:	0f b6 c0             	movzx  eax,al
 7fc:	c1 e0 06             	shl    eax,0x6
 7ff:	44 0b c0             	or     r8d,eax
 802:	48 89 c8             	mov    rax,rcx
 805:	c1 e8 18             	shr    eax,0x18
 808:	25 80 00 00 00       	and    eax,0x80
 80d:	44 0b c0             	or     r8d,eax
 810:	48 89 c8             	mov    rax,rcx
 813:	25 ff 00 00 00       	and    eax,0xff
 818:	f3 0f b8 c0          	popcnt eax,eax
 81c:	83 e0 01             	and    eax,0x1
 81f:	83 f0 01             	xor    eax,0x1
 822:	c1 e0 02             	shl    eax,0x2
 825:	44 0b c0             	or     r8d,eax
 828:	41 8b c1             	mov    eax,r9d
 82b:	41 8b d2             	mov    edx,r10d
 82e:	48 03 c2             	add    rax,rdx
 831:	48 3b 05 a8 03 00 00 	cmp    rax,QWORD PTR [rip+0x3a8]        # 0xbe0
 838:	0f 97 c0             	seta   al
 83b:	0f b6 c0             	movzx  eax,al
 83e:	44 0b c0             	or     r8d,eax
 841:	4c 89 c8             	mov    rax,r9
 844:	41 33 c2             	xor    eax,r10d
 847:	48 89 c2             	mov    rdx,rax
 84a:	33 d1                	xor    edx,ecx
 84c:	83 e2 10             	and    edx,0x10
 84f:	44 0b c2             	or     r8d,edx
 852:	44 33 c9             	xor    r9d,ecx
 855:	c4 c2 78 f2 c1       	andn   eax,eax,r9d
 85a:	c1 e8 14             	shr    eax,0x14
 85d:	25 00 08 00 00       	and    eax,0x800
 862:	44 0b c0             	or     r8d,eax
 865:	48 8b 04 24          	mov    rax,QWORD PTR [rsp]
 869:	44 89 00             	mov    DWORD PTR [rax],r8d
 86c:	48 8b 74 24 38       	mov    rsi,QWORD PTR [rsp+0x38]
 871:	8d 46 0e             	lea    eax,[rsi+0xe]
 874:	48 8b 5c 24 08       	mov    rbx,QWORD PTR [rsp+0x8]
 879:	89 03                	mov    DWORD PTR [rbx],eax
 87b:	4c 8b 64 24 30       	mov    r12,QWORD PTR [rsp+0x30]
 880:	41 8d 4c 24 fa       	lea    ecx,[r12-0x6]
 885:	48 8b 54 24 10       	mov    rdx,QWORD PTR [rsp+0x10]
 88a:	48 89 d0             	mov    rax,rdx
 88d:	2b c1                	sub    eax,ecx
 88f:	48 8b 5c 24 50       	mov    rbx,QWORD PTR [rsp+0x50]
 894:	4c 8b 64 24 58       	mov    r12,QWORD PTR [rsp+0x58]
 899:	4c 8b 6c 24 60       	mov    r13,QWORD PTR [rsp+0x60]
 89e:	4c 8b 74 24 68       	mov    r14,QWORD PTR [rsp+0x68]
 8a3:	4c 8b 7c 24 70       	mov    r15,QWORD PTR [rsp+0x70]
 8a8:	48 81 c4 80 00 00 00 	add    rsp,0x80
 8af:	48 89 ec             	mov    rsp,rbp
 8b2:	5d                   	pop    rbp
 8b3:	c3                   	ret
 8b4:	4c 8b 64 24 30       	mov    r12,QWORD PTR [rsp+0x30]
 8b9:	41 83 fc 07          	cmp    r12d,0x7
 8bd:	0f 85 42 01 00 00    	jne    0xa05
 8c3:	44 89 3f             	mov    DWORD PTR [rdi],r15d
 8c6:	89 4f 04             	mov    DWORD PTR [rdi+0x4],ecx
 8c9:	44 89 6f 08          	mov    DWORD PTR [rdi+0x8],r13d
 8cd:	44 89 57 0c          	mov    DWORD PTR [rdi+0xc],r10d
 8d1:	48 8b 4c 24 48       	mov    rcx,QWORD PTR [rsp+0x48]
 8d6:	89 4f 10             	mov    DWORD PTR [rdi+0x10],ecx
 8d9:	4c 8b 74 24 40       	mov    r14,QWORD PTR [rsp+0x40]
 8de:	44 89 77 14          	mov    DWORD PTR [rdi+0x14],r14d
 8e2:	44 89 5f 18          	mov    DWORD PTR [rdi+0x18],r11d
 8e6:	43 8d 0c 11          	lea    ecx,[r9+r10*1]
 8ea:	89 4f 1c             	mov    DWORD PTR [rdi+0x1c],ecx
 8ed:	41 81 e0 2a f7 ff ff 	and    r8d,0xfffff72a
 8f4:	41 83 c8 02          	or     r8d,0x2
 8f8:	85 c9                	test   ecx,ecx
 8fa:	0f 94 c0             	sete   al
 8fd:	0f b6 c0             	movzx  eax,al
 900:	c1 e0 06             	shl    eax,0x6
 903:	44 0b c0             	or     r8d,eax
 906:	48 89 c8             	mov    rax,rcx
 909:	c1 e8 18             	shr    eax,0x18
 90c:	25 80 00 00 00       	and    eax,0x80
 911:	44 0b c0             	or     r8d,eax
 914:	48 89 c8             	mov    rax,rcx
 917:	25 ff 00 00 00       	and    eax,0xff
 91c:	f3 0f b8 f0          	popcnt esi,eax
 920:	83 e6 01             	and    esi,0x1
 923:	83 f6 01             	xor    esi,0x1
 926:	c1 e6 02             	shl    esi,0x2
 929:	44 0b c6             	or     r8d,esi
 92c:	41 8b c1             	mov    eax,r9d
 92f:	41 8b d2             	mov    edx,r10d
 932:	48 03 c2             	add    rax,rdx
 935:	48 3b 05 a4 02 00 00 	cmp    rax,QWORD PTR [rip+0x2a4]        # 0xbe0
 93c:	0f 97 c0             	seta   al
 93f:	0f b6 c0             	movzx  eax,al
 942:	44 0b c0             	or     r8d,eax
 945:	4c 89 c8             	mov    rax,r9
 948:	41 33 c2             	xor    eax,r10d
 94b:	49 89 c2             	mov    r10,rax
 94e:	44 33 d1             	xor    r10d,ecx
 951:	41 83 e2 10          	and    r10d,0x10
 955:	45 0b c2             	or     r8d,r10d
 958:	44 33 c9             	xor    r9d,ecx
 95b:	c4 42 78 f2 c9       	andn   r9d,eax,r9d
 960:	41 c1 e9 14          	shr    r9d,0x14
 964:	41 81 e1 00 08 00 00 	and    r9d,0x800
 96b:	45 0b c1             	or     r8d,r9d
 96e:	41 81 e0 2a f7 ff ff 	and    r8d,0xfffff72a
 975:	41 83 c8 02          	or     r8d,0x2
 979:	45 85 db             	test   r11d,r11d
 97c:	0f 94 c0             	sete   al
 97f:	44 0f b6 d0          	movzx  r10d,al
 983:	41 c1 e2 06          	shl    r10d,0x6
 987:	45 0b c2             	or     r8d,r10d
 98a:	4d 89 da             	mov    r10,r11
 98d:	41 c1 ea 18          	shr    r10d,0x18
 991:	41 81 e2 80 00 00 00 	and    r10d,0x80
 998:	45 0b c2             	or     r8d,r10d
 99b:	41 81 e3 ff 00 00 00 	and    r11d,0xff
 9a2:	f3 45 0f b8 cb       	popcnt r9d,r11d
 9a7:	41 83 e1 01          	and    r9d,0x1
 9ab:	41 83 f1 01          	xor    r9d,0x1
 9af:	41 c1 e1 02          	shl    r9d,0x2
 9b3:	45 0b c1             	or     r8d,r9d
 9b6:	48 8b 04 24          	mov    rax,QWORD PTR [rsp]
 9ba:	44 89 00             	mov    DWORD PTR [rax],r8d
 9bd:	48 8b 74 24 38       	mov    rsi,QWORD PTR [rsp+0x38]
 9c2:	8d 46 10             	lea    eax,[rsi+0x10]
 9c5:	48 8b 5c 24 08       	mov    rbx,QWORD PTR [rsp+0x8]
 9ca:	89 03                	mov    DWORD PTR [rbx],eax
 9cc:	4c 8b 64 24 30       	mov    r12,QWORD PTR [rsp+0x30]
 9d1:	41 8d 4c 24 f9       	lea    ecx,[r12-0x7]
 9d6:	48 8b 54 24 10       	mov    rdx,QWORD PTR [rsp+0x10]
 9db:	48 89 d0             	mov    rax,rdx
 9de:	2b c1                	sub    eax,ecx
 9e0:	48 8b 5c 24 50       	mov    rbx,QWORD PTR [rsp+0x50]
 9e5:	4c 8b 64 24 58       	mov    r12,QWORD PTR [rsp+0x58]
 9ea:	4c 8b 6c 24 60       	mov    r13,QWORD PTR [rsp+0x60]
 9ef:	4c 8b 74 24 68       	mov    r14,QWORD PTR [rsp+0x68]
 9f4:	4c 8b 7c 24 70       	mov    r15,QWORD PTR [rsp+0x70]
 9f9:	48 81 c4 80 00 00 00 	add    rsp,0x80
 a00:	48 89 ec             	mov    rsp,rbp
 a03:	5d                   	pop    rbp
 a04:	c3                   	ret
 a05:	41 81 e0 2a f7 ff ff 	and    r8d,0xfffff72a
 a0c:	41 83 c8 02          	or     r8d,0x2
 a10:	43 8d 14 11          	lea    edx,[r9+r10*1]
 a14:	85 d2                	test   edx,edx
 a16:	0f 94 c0             	sete   al
 a19:	0f b6 c0             	movzx  eax,al
 a1c:	c1 e0 06             	shl    eax,0x6
 a1f:	44 0b c0             	or     r8d,eax
 a22:	48 89 d0             	mov    rax,rdx
 a25:	c1 e8 18             	shr    eax,0x18
 a28:	25 80 00 00 00       	and    eax,0x80
 a2d:	44 0b c0             	or     r8d,eax
 a30:	48 89 d0             	mov    rax,rdx
 a33:	25 ff 00 00 00       	and    eax,0xff
 a38:	f3 0f b8 c0          	popcnt eax,eax
 a3c:	83 e0 01             	and    eax,0x1
 a3f:	83 f0 01             	xor    eax,0x1
 a42:	c1 e0 02             	shl    eax,0x2
 a45:	44 0b c0             	or     r8d,eax
 a48:	41 8b c1             	mov    eax,r9d
 a4b:	41 8b f2             	mov    esi,r10d
 a4e:	48 03 c6             	add    rax,rsi
 a51:	48 3b 05 88 01 00 00 	cmp    rax,QWORD PTR [rip+0x188]        # 0xbe0
 a58:	0f 97 c0             	seta   al
 a5b:	0f b6 c0             	movzx  eax,al
 a5e:	44 0b c0             	or     r8d,eax
 a61:	4c 89 c8             	mov    rax,r9
 a64:	41 33 c2             	xor    eax,r10d
 a67:	48 89 c6             	mov    rsi,rax
 a6a:	33 f2                	xor    esi,edx
 a6c:	83 e6 10             	and    esi,0x10
 a6f:	44 0b c6             	or     r8d,esi
 a72:	44 33 ca             	xor    r9d,edx
 a75:	c4 c2 78 f2 c1       	andn   eax,eax,r9d
 a7a:	c1 e8 14             	shr    eax,0x14
 a7d:	25 00 08 00 00       	and    eax,0x800
 a82:	44 0b c0             	or     r8d,eax
 a85:	41 81 e0 2a f7 ff ff 	and    r8d,0xfffff72a
 a8c:	41 83 c8 02          	or     r8d,0x2
 a90:	45 85 db             	test   r11d,r11d
 a93:	0f 94 c0             	sete   al
 a96:	0f b6 c0             	movzx  eax,al
 a99:	c1 e0 06             	shl    eax,0x6
 a9c:	44 0b c0             	or     r8d,eax
 a9f:	4c 89 d8             	mov    rax,r11
 aa2:	c1 e8 18             	shr    eax,0x18
 aa5:	25 80 00 00 00       	and    eax,0x80
 aaa:	44 0b c0             	or     r8d,eax
 aad:	4c 89 d8             	mov    rax,r11
 ab0:	25 ff 00 00 00       	and    eax,0xff
 ab5:	f3 0f b8 c0          	popcnt eax,eax
 ab9:	83 e0 01             	and    eax,0x1
 abc:	83 f0 01             	xor    eax,0x1
 abf:	c1 e0 02             	shl    eax,0x2
 ac2:	44 0b c0             	or     r8d,eax
 ac5:	41 f7 c0 40 00 00 00 	test   r8d,0x40
 acc:	0f 84 75 00 00 00    	je     0xb47
 ad2:	44 89 3f             	mov    DWORD PTR [rdi],r15d
 ad5:	89 4f 04             	mov    DWORD PTR [rdi+0x4],ecx
 ad8:	44 89 6f 08          	mov    DWORD PTR [rdi+0x8],r13d
 adc:	44 89 57 0c          	mov    DWORD PTR [rdi+0xc],r10d
 ae0:	48 8b 4c 24 48       	mov    rcx,QWORD PTR [rsp+0x48]
 ae5:	89 4f 10             	mov    DWORD PTR [rdi+0x10],ecx
 ae8:	4c 8b 74 24 40       	mov    r14,QWORD PTR [rsp+0x40]
 aed:	44 89 77 14          	mov    DWORD PTR [rdi+0x14],r14d
 af1:	44 89 5f 18          	mov    DWORD PTR [rdi+0x18],r11d
 af5:	89 57 1c             	mov    DWORD PTR [rdi+0x1c],edx
 af8:	48 8b 04 24          	mov    rax,QWORD PTR [rsp]
 afc:	44 89 00             	mov    DWORD PTR [rax],r8d
 aff:	48 8b 74 24 38       	mov    rsi,QWORD PTR [rsp+0x38]
 b04:	8d 46 12             	lea    eax,[rsi+0x12]
 b07:	48 8b 5c 24 08       	mov    rbx,QWORD PTR [rsp+0x8]
 b0c:	89 03                	mov    DWORD PTR [rbx],eax
 b0e:	4c 8b 64 24 30       	mov    r12,QWORD PTR [rsp+0x30]
 b13:	41 8d 4c 24 f8       	lea    ecx,[r12-0x8]
 b18:	48 8b 54 24 10       	mov    rdx,QWORD PTR [rsp+0x10]
 b1d:	48 89 d0             	mov    rax,rdx
 b20:	2b c1                	sub    eax,ecx
 b22:	48 8b 5c 24 50       	mov    rbx,QWORD PTR [rsp+0x50]
 b27:	4c 8b 64 24 58       	mov    r12,QWORD PTR [rsp+0x58]
 b2c:	4c 8b 6c 24 60       	mov    r13,QWORD PTR [rsp+0x60]
 b31:	4c 8b 74 24 68       	mov    r14,QWORD PTR [rsp+0x68]
 b36:	4c 8b 7c 24 70       	mov    r15,QWORD PTR [rsp+0x70]
 b3b:	48 81 c4 80 00 00 00 	add    rsp,0x80
 b42:	48 89 ec             	mov    rsp,rbp
 b45:	5d                   	pop    rbp
 b46:	c3                   	ret
 b47:	4c 8b 64 24 30       	mov    r12,QWORD PTR [rsp+0x30]
 b4c:	41 81 c4 f8 ff ff ff 	add    r12d,0xfffffff8
 b53:	4c 89 64 24 30       	mov    QWORD PTR [rsp+0x30],r12
 b58:	49 89 d1             	mov    r9,rdx
 b5b:	e9 1a f5 ff ff       	jmp    0x7a
 b60:	44 89 3f             	mov    DWORD PTR [rdi],r15d
 b63:	89 4f 04             	mov    DWORD PTR [rdi+0x4],ecx
 b66:	44 89 6f 08          	mov    DWORD PTR [rdi+0x8],r13d
 b6a:	44 89 57 0c          	mov    DWORD PTR [rdi+0xc],r10d
 b6e:	48 8b 4c 24 48       	mov    rcx,QWORD PTR [rsp+0x48]
 b73:	89 4f 10             	mov    DWORD PTR [rdi+0x10],ecx
 b76:	4c 8b 74 24 40       	mov    r14,QWORD PTR [rsp+0x40]
 b7b:	44 89 77 14          	mov    DWORD PTR [rdi+0x14],r14d
 b7f:	44 89 5f 18          	mov    DWORD PTR [rdi+0x18],r11d
 b83:	44 89 4f 1c          	mov    DWORD PTR [rdi+0x1c],r9d
 b87:	48 8b 04 24          	mov    rax,QWORD PTR [rsp]
 b8b:	44 89 00             	mov    DWORD PTR [rax],r8d
 b8e:	48 8b 74 24 38       	mov    rsi,QWORD PTR [rsp+0x38]
 b93:	8d 46 12             	lea    eax,[rsi+0x12]
 b96:	48 8b 5c 24 08       	mov    rbx,QWORD PTR [rsp+0x8]
 b9b:	89 03                	mov    DWORD PTR [rbx],eax
 b9d:	4c 8b 64 24 30       	mov    r12,QWORD PTR [rsp+0x30]
 ba2:	41 8d 4c 24 fd       	lea    ecx,[r12-0x3]
 ba7:	48 8b 54 24 10       	mov    rdx,QWORD PTR [rsp+0x10]
 bac:	48 89 d0             	mov    rax,rdx
 baf:	2b c1                	sub    eax,ecx
 bb1:	48 8b 5c 24 50       	mov    rbx,QWORD PTR [rsp+0x50]
 bb6:	4c 8b 64 24 58       	mov    r12,QWORD PTR [rsp+0x58]
 bbb:	4c 8b 6c 24 60       	mov    r13,QWORD PTR [rsp+0x60]
 bc0:	4c 8b 74 24 68       	mov    r14,QWORD PTR [rsp+0x68]
 bc5:	4c 8b 7c 24 70       	mov    r15,QWORD PTR [rsp+0x70]
 bca:	48 81 c4 80 00 00 00 	add    rsp,0x80
 bd1:	48 89 ec             	mov    rsp,rbp
 bd4:	5d                   	pop    rbp
 bd5:	c3                   	ret
 bd6:	00 00                	add    BYTE PTR [rax],al
 bd8:	fc                   	cld
 bd9:	ff                   	(bad)
 bda:	ff                   	(bad)
 bdb:	ff 00                	inc    DWORD PTR [rax]
 bdd:	00 00                	add    BYTE PTR [rax],al
 bdf:	00 ff                	add    bh,bh
 be1:	ff                   	(bad)
 be2:	ff                   	(bad)
 be3:	ff 00                	inc    DWORD PTR [rax]
 be5:	00 00                	add    BYTE PTR [rax],al
	...
