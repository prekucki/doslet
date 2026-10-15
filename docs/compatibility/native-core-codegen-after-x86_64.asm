# Actual host code for the original synthetic linked-list trace, not game code.
# 1816 native bytes including constants; shared exit and conservative dirty GPR mask.

/tmp/doslet-ram-trace-after.bin:     file format binary


Disassembly of section .data:

0000000000000000 <.data>:
   0:	55                   	push   rbp
   1:	48 89 e5             	mov    rbp,rsp
   4:	48 83 ec 60          	sub    rsp,0x60
   8:	48 89 5c 24 30       	mov    QWORD PTR [rsp+0x30],rbx
   d:	4c 89 64 24 38       	mov    QWORD PTR [rsp+0x38],r12
  12:	4c 89 6c 24 40       	mov    QWORD PTR [rsp+0x40],r13
  17:	4c 89 74 24 48       	mov    QWORD PTR [rsp+0x48],r14
  1c:	4c 89 7c 24 50       	mov    QWORD PTR [rsp+0x50],r15
  21:	48 89 4c 24 18       	mov    QWORD PTR [rsp+0x18],rcx
  26:	4c 89 44 24 20       	mov    QWORD PTR [rsp+0x20],r8
  2b:	4c 89 4c 24 28       	mov    QWORD PTR [rsp+0x28],r9
  30:	4c 8b 75 10          	mov    r14,QWORD PTR [rbp+0x10]
  34:	8b 07                	mov    eax,DWORD PTR [rdi]
  36:	8b 4f 04             	mov    ecx,DWORD PTR [rdi+0x4]
  39:	44 8b 6f 08          	mov    r13d,DWORD PTR [rdi+0x8]
  3d:	44 8b 47 0c          	mov    r8d,DWORD PTR [rdi+0xc]
  41:	8b 4f 10             	mov    ecx,DWORD PTR [rdi+0x10]
  44:	8b 4f 14             	mov    ecx,DWORD PTR [rdi+0x14]
  47:	8b 5f 18             	mov    ebx,DWORD PTR [rdi+0x18]
  4a:	44 8b 67 1c          	mov    r12d,DWORD PTR [rdi+0x1c]
  4e:	48 89 3c 24          	mov    QWORD PTR [rsp],rdi
  52:	44 8b 0e             	mov    r9d,DWORD PTR [rsi]
  55:	48 89 74 24 08       	mov    QWORD PTR [rsp+0x8],rsi
  5a:	44 8b 1a             	mov    r11d,DWORD PTR [rdx]
  5d:	48 89 54 24 10       	mov    QWORD PTR [rsp+0x10],rdx
  62:	4c 8b 54 24 18       	mov    r10,QWORD PTR [rsp+0x18]
  67:	45 85 d2             	test   r10d,r10d
  6a:	0f 85 09 00 00 00    	jne    0x79
  70:	48 8b 3c 24          	mov    rdi,QWORD PTR [rsp]
  74:	e9 42 06 00 00       	jmp    0x6bb
  79:	8b cb                	mov    ecx,ebx
  7b:	48 8b 7c 24 20       	mov    rdi,QWORD PTR [rsp+0x20]
  80:	8b d7                	mov    edx,edi
  82:	48 3b ca             	cmp    rcx,rdx
  85:	41 0f 93 c7          	setae  r15b
  89:	48 89 ce             	mov    rsi,rcx
  8c:	48 2b f2             	sub    rsi,rdx
  8f:	48 8d 7e 04          	lea    rdi,[rsi+0x4]
  93:	49 3b fe             	cmp    rdi,r14
  96:	40 0f 96 c7          	setbe  dil
  9a:	48 3b 0d 67 06 00 00 	cmp    rcx,QWORD PTR [rip+0x667]        # 0x708
  a1:	0f 96 c1             	setbe  cl
  a4:	23 f9                	and    edi,ecx
  a6:	44 84 ff             	test   dil,r15b
  a9:	0f 85 09 00 00 00    	jne    0xb8
  af:	48 8b 3c 24          	mov    rdi,QWORD PTR [rsp]
  b3:	e9 03 06 00 00       	jmp    0x6bb
  b8:	48 8b 4c 24 28       	mov    rcx,QWORD PTR [rsp+0x28]
  bd:	8b 04 31             	mov    eax,DWORD PTR [rcx+rsi*1]
  c0:	41 83 fa 01          	cmp    r10d,0x1
  c4:	0f 85 17 00 00 00    	jne    0xe1
  ca:	41 81 c3 02 00 00 00 	add    r11d,0x2
  d1:	41 81 c2 ff ff ff ff 	add    r10d,0xffffffff
  d8:	48 8b 3c 24          	mov    rdi,QWORD PTR [rsp]
  dc:	e9 da 05 00 00       	jmp    0x6bb
  e1:	41 83 fa 02          	cmp    r10d,0x2
  e5:	0f 85 93 00 00 00    	jne    0x17e
  eb:	41 81 e1 2a f7 ff ff 	and    r9d,0xfffff72a
  f2:	41 83 c9 02          	or     r9d,0x2
  f6:	48 89 c1             	mov    rcx,rax
  f9:	41 2b cd             	sub    ecx,r13d
  fc:	85 c9                	test   ecx,ecx
  fe:	0f 94 c2             	sete   dl
 101:	0f b6 d2             	movzx  edx,dl
 104:	c1 e2 06             	shl    edx,0x6
 107:	44 0b ca             	or     r9d,edx
 10a:	48 89 ca             	mov    rdx,rcx
 10d:	c1 ea 18             	shr    edx,0x18
 110:	81 e2 80 00 00 00    	and    edx,0x80
 116:	44 0b ca             	or     r9d,edx
 119:	48 89 ca             	mov    rdx,rcx
 11c:	81 e2 ff 00 00 00    	and    edx,0xff
 122:	f3 0f b8 d2          	popcnt edx,edx
 126:	83 e2 01             	and    edx,0x1
 129:	83 f2 01             	xor    edx,0x1
 12c:	c1 e2 02             	shl    edx,0x2
 12f:	44 0b ca             	or     r9d,edx
 132:	8b d0                	mov    edx,eax
 134:	41 8b f5             	mov    esi,r13d
 137:	48 3b d6             	cmp    rdx,rsi
 13a:	0f 92 c2             	setb   dl
 13d:	0f b6 d2             	movzx  edx,dl
 140:	44 0b ca             	or     r9d,edx
 143:	48 89 c2             	mov    rdx,rax
 146:	41 33 d5             	xor    edx,r13d
 149:	48 89 d6             	mov    rsi,rdx
 14c:	33 f1                	xor    esi,ecx
 14e:	83 e6 10             	and    esi,0x10
 151:	44 0b ce             	or     r9d,esi
 154:	48 89 c6             	mov    rsi,rax
 157:	33 f1                	xor    esi,ecx
 159:	23 d6                	and    edx,esi
 15b:	c1 ea 14             	shr    edx,0x14
 15e:	81 e2 00 08 00 00    	and    edx,0x800
 164:	44 0b ca             	or     r9d,edx
 167:	41 81 c3 04 00 00 00 	add    r11d,0x4
 16e:	41 81 c2 fe ff ff ff 	add    r10d,0xfffffffe
 175:	48 8b 3c 24          	mov    rdi,QWORD PTR [rsp]
 179:	e9 3d 05 00 00       	jmp    0x6bb
 17e:	41 81 e1 2a f7 ff ff 	and    r9d,0xfffff72a
 185:	41 83 c9 02          	or     r9d,0x2
 189:	48 89 c6             	mov    rsi,rax
 18c:	41 2b f5             	sub    esi,r13d
 18f:	85 f6                	test   esi,esi
 191:	0f 94 c1             	sete   cl
 194:	0f b6 c9             	movzx  ecx,cl
 197:	c1 e1 06             	shl    ecx,0x6
 19a:	44 0b c9             	or     r9d,ecx
 19d:	48 89 f1             	mov    rcx,rsi
 1a0:	c1 e9 18             	shr    ecx,0x18
 1a3:	81 e1 80 00 00 00    	and    ecx,0x80
 1a9:	44 0b c9             	or     r9d,ecx
 1ac:	48 89 f1             	mov    rcx,rsi
 1af:	81 e1 ff 00 00 00    	and    ecx,0xff
 1b5:	f3 0f b8 f9          	popcnt edi,ecx
 1b9:	83 e7 01             	and    edi,0x1
 1bc:	83 f7 01             	xor    edi,0x1
 1bf:	c1 e7 02             	shl    edi,0x2
 1c2:	44 0b cf             	or     r9d,edi
 1c5:	8b c8                	mov    ecx,eax
 1c7:	41 8b fd             	mov    edi,r13d
 1ca:	48 3b cf             	cmp    rcx,rdi
 1cd:	0f 92 c1             	setb   cl
 1d0:	0f b6 c9             	movzx  ecx,cl
 1d3:	44 0b c9             	or     r9d,ecx
 1d6:	49 89 c7             	mov    r15,rax
 1d9:	45 33 fd             	xor    r15d,r13d
 1dc:	4c 89 f9             	mov    rcx,r15
 1df:	33 ce                	xor    ecx,esi
 1e1:	83 e1 10             	and    ecx,0x10
 1e4:	44 0b c9             	or     r9d,ecx
 1e7:	48 89 c1             	mov    rcx,rax
 1ea:	33 ce                	xor    ecx,esi
 1ec:	44 23 f9             	and    r15d,ecx
 1ef:	41 c1 ef 14          	shr    r15d,0x14
 1f3:	41 81 e7 00 08 00 00 	and    r15d,0x800
 1fa:	45 0b cf             	or     r9d,r15d
 1fd:	41 f7 c1 40 00 00 00 	test   r9d,0x40
 204:	0f 85 9f 04 00 00    	jne    0x6a9
 20a:	41 83 fa 03          	cmp    r10d,0x3
 20e:	0f 85 17 00 00 00    	jne    0x22b
 214:	41 81 c3 06 00 00 00 	add    r11d,0x6
 21b:	41 81 c2 fd ff ff ff 	add    r10d,0xfffffffd
 222:	48 8b 3c 24          	mov    rdi,QWORD PTR [rsp]
 226:	e9 90 04 00 00       	jmp    0x6bb
 22b:	8d 7b 04             	lea    edi,[rbx+0x4]
 22e:	48 3b fa             	cmp    rdi,rdx
 231:	41 0f 93 c7          	setae  r15b
 235:	48 89 fe             	mov    rsi,rdi
 238:	48 2b f2             	sub    rsi,rdx
 23b:	48 8d 4e 04          	lea    rcx,[rsi+0x4]
 23f:	49 3b ce             	cmp    rcx,r14
 242:	0f 96 c1             	setbe  cl
 245:	48 3b 3d bc 04 00 00 	cmp    rdi,QWORD PTR [rip+0x4bc]        # 0x708
 24c:	40 0f 96 c7          	setbe  dil
 250:	23 cf                	and    ecx,edi
 252:	44 84 f9             	test   cl,r15b
 255:	0f 85 17 00 00 00    	jne    0x272
 25b:	41 81 c3 06 00 00 00 	add    r11d,0x6
 262:	41 81 c2 fd ff ff ff 	add    r10d,0xfffffffd
 269:	48 8b 3c 24          	mov    rdi,QWORD PTR [rsp]
 26d:	e9 49 04 00 00       	jmp    0x6bb
 272:	48 8b 4c 24 28       	mov    rcx,QWORD PTR [rsp+0x28]
 277:	44 8b 04 31          	mov    r8d,DWORD PTR [rcx+rsi*1]
 27b:	41 83 fa 04          	cmp    r10d,0x4
 27f:	0f 85 17 00 00 00    	jne    0x29c
 285:	41 81 c3 09 00 00 00 	add    r11d,0x9
 28c:	41 81 c2 fc ff ff ff 	add    r10d,0xfffffffc
 293:	48 8b 3c 24          	mov    rdi,QWORD PTR [rsp]
 297:	e9 1f 04 00 00       	jmp    0x6bb
 29c:	41 83 fa 05          	cmp    r10d,0x5
 2a0:	0f 85 9f 00 00 00    	jne    0x345
 2a6:	43 8d 0c 04          	lea    ecx,[r12+r8*1]
 2aa:	41 81 e1 2a f7 ff ff 	and    r9d,0xfffff72a
 2b1:	41 83 c9 02          	or     r9d,0x2
 2b5:	85 c9                	test   ecx,ecx
 2b7:	0f 94 c2             	sete   dl
 2ba:	0f b6 d2             	movzx  edx,dl
 2bd:	c1 e2 06             	shl    edx,0x6
 2c0:	44 0b ca             	or     r9d,edx
 2c3:	48 89 ca             	mov    rdx,rcx
 2c6:	c1 ea 18             	shr    edx,0x18
 2c9:	81 e2 80 00 00 00    	and    edx,0x80
 2cf:	44 0b ca             	or     r9d,edx
 2d2:	48 89 ca             	mov    rdx,rcx
 2d5:	81 e2 ff 00 00 00    	and    edx,0xff
 2db:	f3 0f b8 d2          	popcnt edx,edx
 2df:	83 e2 01             	and    edx,0x1
 2e2:	83 f2 01             	xor    edx,0x1
 2e5:	c1 e2 02             	shl    edx,0x2
 2e8:	44 0b ca             	or     r9d,edx
 2eb:	41 8b d4             	mov    edx,r12d
 2ee:	41 8b f0             	mov    esi,r8d
 2f1:	48 03 d6             	add    rdx,rsi
 2f4:	48 3b 15 15 04 00 00 	cmp    rdx,QWORD PTR [rip+0x415]        # 0x710
 2fb:	40 0f 97 c6          	seta   sil
 2ff:	40 0f b6 f6          	movzx  esi,sil
 303:	44 0b ce             	or     r9d,esi
 306:	4c 89 e6             	mov    rsi,r12
 309:	41 33 f0             	xor    esi,r8d
 30c:	48 89 f2             	mov    rdx,rsi
 30f:	33 d1                	xor    edx,ecx
 311:	83 e2 10             	and    edx,0x10
 314:	44 0b ca             	or     r9d,edx
 317:	44 33 e1             	xor    r12d,ecx
 31a:	c4 c2 48 f2 d4       	andn   edx,esi,r12d
 31f:	c1 ea 14             	shr    edx,0x14
 322:	81 e2 00 08 00 00    	and    edx,0x800
 328:	44 0b ca             	or     r9d,edx
 32b:	41 81 c3 0b 00 00 00 	add    r11d,0xb
 332:	41 81 c2 fb ff ff ff 	add    r10d,0xfffffffb
 339:	48 8b 3c 24          	mov    rdi,QWORD PTR [rsp]
 33d:	49 89 cc             	mov    r12,rcx
 340:	e9 76 03 00 00       	jmp    0x6bb
 345:	8d 7b 08             	lea    edi,[rbx+0x8]
 348:	48 3b fa             	cmp    rdi,rdx
 34b:	41 0f 93 c7          	setae  r15b
 34f:	48 89 fe             	mov    rsi,rdi
 352:	48 2b f2             	sub    rsi,rdx
 355:	48 8d 56 04          	lea    rdx,[rsi+0x4]
 359:	49 3b d6             	cmp    rdx,r14
 35c:	0f 96 c2             	setbe  dl
 35f:	48 3b 3d a2 03 00 00 	cmp    rdi,QWORD PTR [rip+0x3a2]        # 0x708
 366:	40 0f 96 c7          	setbe  dil
 36a:	23 d7                	and    edx,edi
 36c:	44 84 fa             	test   dl,r15b
 36f:	0f 85 9d 00 00 00    	jne    0x412
 375:	43 8d 0c 04          	lea    ecx,[r12+r8*1]
 379:	41 81 e1 2a f7 ff ff 	and    r9d,0xfffff72a
 380:	41 83 c9 02          	or     r9d,0x2
 384:	85 c9                	test   ecx,ecx
 386:	0f 94 c2             	sete   dl
 389:	0f b6 d2             	movzx  edx,dl
 38c:	c1 e2 06             	shl    edx,0x6
 38f:	44 0b ca             	or     r9d,edx
 392:	48 89 ca             	mov    rdx,rcx
 395:	c1 ea 18             	shr    edx,0x18
 398:	81 e2 80 00 00 00    	and    edx,0x80
 39e:	44 0b ca             	or     r9d,edx
 3a1:	48 89 ca             	mov    rdx,rcx
 3a4:	81 e2 ff 00 00 00    	and    edx,0xff
 3aa:	f3 0f b8 fa          	popcnt edi,edx
 3ae:	83 e7 01             	and    edi,0x1
 3b1:	83 f7 01             	xor    edi,0x1
 3b4:	c1 e7 02             	shl    edi,0x2
 3b7:	44 0b cf             	or     r9d,edi
 3ba:	41 8b d4             	mov    edx,r12d
 3bd:	41 8b f0             	mov    esi,r8d
 3c0:	48 03 d6             	add    rdx,rsi
 3c3:	48 3b 15 46 03 00 00 	cmp    rdx,QWORD PTR [rip+0x346]        # 0x710
 3ca:	0f 97 c2             	seta   dl
 3cd:	0f b6 d2             	movzx  edx,dl
 3d0:	44 0b ca             	or     r9d,edx
 3d3:	4c 89 e2             	mov    rdx,r12
 3d6:	41 33 d0             	xor    edx,r8d
 3d9:	48 89 d6             	mov    rsi,rdx
 3dc:	33 f1                	xor    esi,ecx
 3de:	83 e6 10             	and    esi,0x10
 3e1:	44 0b ce             	or     r9d,esi
 3e4:	44 33 e1             	xor    r12d,ecx
 3e7:	c4 c2 68 f2 d4       	andn   edx,edx,r12d
 3ec:	c1 ea 14             	shr    edx,0x14
 3ef:	81 e2 00 08 00 00    	and    edx,0x800
 3f5:	44 0b ca             	or     r9d,edx
 3f8:	41 81 c3 0b 00 00 00 	add    r11d,0xb
 3ff:	41 81 c2 fb ff ff ff 	add    r10d,0xfffffffb
 406:	48 8b 3c 24          	mov    rdi,QWORD PTR [rsp]
 40a:	49 89 cc             	mov    r12,rcx
 40d:	e9 a9 02 00 00       	jmp    0x6bb
 412:	8b 1c 31             	mov    ebx,DWORD PTR [rcx+rsi*1]
 415:	41 83 fa 06          	cmp    r10d,0x6
 419:	0f 85 9d 00 00 00    	jne    0x4bc
 41f:	43 8d 0c 04          	lea    ecx,[r12+r8*1]
 423:	41 81 e1 2a f7 ff ff 	and    r9d,0xfffff72a
 42a:	41 83 c9 02          	or     r9d,0x2
 42e:	85 c9                	test   ecx,ecx
 430:	0f 94 c2             	sete   dl
 433:	0f b6 d2             	movzx  edx,dl
 436:	c1 e2 06             	shl    edx,0x6
 439:	44 0b ca             	or     r9d,edx
 43c:	48 89 ca             	mov    rdx,rcx
 43f:	c1 ea 18             	shr    edx,0x18
 442:	81 e2 80 00 00 00    	and    edx,0x80
 448:	44 0b ca             	or     r9d,edx
 44b:	48 89 ca             	mov    rdx,rcx
 44e:	81 e2 ff 00 00 00    	and    edx,0xff
 454:	f3 0f b8 d2          	popcnt edx,edx
 458:	83 e2 01             	and    edx,0x1
 45b:	83 f2 01             	xor    edx,0x1
 45e:	c1 e2 02             	shl    edx,0x2
 461:	44 0b ca             	or     r9d,edx
 464:	41 8b d4             	mov    edx,r12d
 467:	41 8b f0             	mov    esi,r8d
 46a:	48 03 d6             	add    rdx,rsi
 46d:	48 3b 15 9c 02 00 00 	cmp    rdx,QWORD PTR [rip+0x29c]        # 0x710
 474:	0f 97 c2             	seta   dl
 477:	0f b6 d2             	movzx  edx,dl
 47a:	44 0b ca             	or     r9d,edx
 47d:	4c 89 e2             	mov    rdx,r12
 480:	41 33 d0             	xor    edx,r8d
 483:	48 89 d6             	mov    rsi,rdx
 486:	33 f1                	xor    esi,ecx
 488:	83 e6 10             	and    esi,0x10
 48b:	44 0b ce             	or     r9d,esi
 48e:	44 33 e1             	xor    r12d,ecx
 491:	c4 c2 68 f2 d4       	andn   edx,edx,r12d
 496:	c1 ea 14             	shr    edx,0x14
 499:	81 e2 00 08 00 00    	and    edx,0x800
 49f:	44 0b ca             	or     r9d,edx
 4a2:	41 81 c3 0e 00 00 00 	add    r11d,0xe
 4a9:	41 81 c2 fa ff ff ff 	add    r10d,0xfffffffa
 4b0:	48 8b 3c 24          	mov    rdi,QWORD PTR [rsp]
 4b4:	49 89 cc             	mov    r12,rcx
 4b7:	e9 ff 01 00 00       	jmp    0x6bb
 4bc:	41 83 fa 07          	cmp    r10d,0x7
 4c0:	0f 85 de 00 00 00    	jne    0x5a4
 4c6:	43 8d 0c 04          	lea    ecx,[r12+r8*1]
 4ca:	41 81 e1 2a f7 ff ff 	and    r9d,0xfffff72a
 4d1:	41 83 c9 02          	or     r9d,0x2
 4d5:	85 c9                	test   ecx,ecx
 4d7:	0f 94 c2             	sete   dl
 4da:	0f b6 d2             	movzx  edx,dl
 4dd:	c1 e2 06             	shl    edx,0x6
 4e0:	44 0b ca             	or     r9d,edx
 4e3:	48 89 ca             	mov    rdx,rcx
 4e6:	c1 ea 18             	shr    edx,0x18
 4e9:	81 e2 80 00 00 00    	and    edx,0x80
 4ef:	44 0b ca             	or     r9d,edx
 4f2:	48 89 ca             	mov    rdx,rcx
 4f5:	81 e2 ff 00 00 00    	and    edx,0xff
 4fb:	f3 0f b8 d2          	popcnt edx,edx
 4ff:	83 e2 01             	and    edx,0x1
 502:	83 f2 01             	xor    edx,0x1
 505:	c1 e2 02             	shl    edx,0x2
 508:	44 0b ca             	or     r9d,edx
 50b:	41 8b d4             	mov    edx,r12d
 50e:	41 8b f0             	mov    esi,r8d
 511:	48 03 d6             	add    rdx,rsi
 514:	48 3b 15 f5 01 00 00 	cmp    rdx,QWORD PTR [rip+0x1f5]        # 0x710
 51b:	0f 97 c2             	seta   dl
 51e:	0f b6 d2             	movzx  edx,dl
 521:	44 0b ca             	or     r9d,edx
 524:	4c 89 e2             	mov    rdx,r12
 527:	41 33 d0             	xor    edx,r8d
 52a:	48 89 d6             	mov    rsi,rdx
 52d:	33 f1                	xor    esi,ecx
 52f:	83 e6 10             	and    esi,0x10
 532:	44 0b ce             	or     r9d,esi
 535:	44 33 e1             	xor    r12d,ecx
 538:	c4 c2 68 f2 d4       	andn   edx,edx,r12d
 53d:	c1 ea 14             	shr    edx,0x14
 540:	81 e2 00 08 00 00    	and    edx,0x800
 546:	44 0b ca             	or     r9d,edx
 549:	41 81 e1 2a f7 ff ff 	and    r9d,0xfffff72a
 550:	41 83 c9 02          	or     r9d,0x2
 554:	85 db                	test   ebx,ebx
 556:	0f 94 c2             	sete   dl
 559:	0f b6 d2             	movzx  edx,dl
 55c:	c1 e2 06             	shl    edx,0x6
 55f:	44 0b ca             	or     r9d,edx
 562:	48 89 da             	mov    rdx,rbx
 565:	c1 ea 18             	shr    edx,0x18
 568:	81 e2 80 00 00 00    	and    edx,0x80
 56e:	44 0b ca             	or     r9d,edx
 571:	48 89 da             	mov    rdx,rbx
 574:	81 e2 ff 00 00 00    	and    edx,0xff
 57a:	f3 0f b8 d2          	popcnt edx,edx
 57e:	83 e2 01             	and    edx,0x1
 581:	83 f2 01             	xor    edx,0x1
 584:	c1 e2 02             	shl    edx,0x2
 587:	44 0b ca             	or     r9d,edx
 58a:	41 81 c3 10 00 00 00 	add    r11d,0x10
 591:	41 81 c2 f9 ff ff ff 	add    r10d,0xfffffff9
 598:	48 8b 3c 24          	mov    rdi,QWORD PTR [rsp]
 59c:	49 89 cc             	mov    r12,rcx
 59f:	e9 17 01 00 00       	jmp    0x6bb
 5a4:	41 81 e1 2a f7 ff ff 	and    r9d,0xfffff72a
 5ab:	41 83 c9 02          	or     r9d,0x2
 5af:	43 8d 14 04          	lea    edx,[r12+r8*1]
 5b3:	85 d2                	test   edx,edx
 5b5:	40 0f 94 c6          	sete   sil
 5b9:	40 0f b6 f6          	movzx  esi,sil
 5bd:	c1 e6 06             	shl    esi,0x6
 5c0:	44 0b ce             	or     r9d,esi
 5c3:	48 89 d6             	mov    rsi,rdx
 5c6:	c1 ee 18             	shr    esi,0x18
 5c9:	81 e6 80 00 00 00    	and    esi,0x80
 5cf:	44 0b ce             	or     r9d,esi
 5d2:	48 89 d6             	mov    rsi,rdx
 5d5:	81 e6 ff 00 00 00    	and    esi,0xff
 5db:	f3 0f b8 f6          	popcnt esi,esi
 5df:	83 e6 01             	and    esi,0x1
 5e2:	83 f6 01             	xor    esi,0x1
 5e5:	c1 e6 02             	shl    esi,0x2
 5e8:	44 0b ce             	or     r9d,esi
 5eb:	41 8b f4             	mov    esi,r12d
 5ee:	41 8b f8             	mov    edi,r8d
 5f1:	48 03 f7             	add    rsi,rdi
 5f4:	48 3b 35 15 01 00 00 	cmp    rsi,QWORD PTR [rip+0x115]        # 0x710
 5fb:	40 0f 97 c6          	seta   sil
 5ff:	40 0f b6 f6          	movzx  esi,sil
 603:	44 0b ce             	or     r9d,esi
 606:	4c 89 e6             	mov    rsi,r12
 609:	41 33 f0             	xor    esi,r8d
 60c:	48 89 f7             	mov    rdi,rsi
 60f:	33 fa                	xor    edi,edx
 611:	83 e7 10             	and    edi,0x10
 614:	44 0b cf             	or     r9d,edi
 617:	44 33 e2             	xor    r12d,edx
 61a:	c4 c2 48 f2 f4       	andn   esi,esi,r12d
 61f:	c1 ee 14             	shr    esi,0x14
 622:	81 e6 00 08 00 00    	and    esi,0x800
 628:	44 0b ce             	or     r9d,esi
 62b:	41 81 e1 2a f7 ff ff 	and    r9d,0xfffff72a
 632:	41 83 c9 02          	or     r9d,0x2
 636:	85 db                	test   ebx,ebx
 638:	40 0f 94 c6          	sete   sil
 63c:	40 0f b6 f6          	movzx  esi,sil
 640:	c1 e6 06             	shl    esi,0x6
 643:	44 0b ce             	or     r9d,esi
 646:	48 89 de             	mov    rsi,rbx
 649:	c1 ee 18             	shr    esi,0x18
 64c:	81 e6 80 00 00 00    	and    esi,0x80
 652:	44 0b ce             	or     r9d,esi
 655:	48 89 de             	mov    rsi,rbx
 658:	81 e6 ff 00 00 00    	and    esi,0xff
 65e:	f3 0f b8 f6          	popcnt esi,esi
 662:	83 e6 01             	and    esi,0x1
 665:	83 f6 01             	xor    esi,0x1
 668:	c1 e6 02             	shl    esi,0x2
 66b:	44 0b ce             	or     r9d,esi
 66e:	41 f7 c1 40 00 00 00 	test   r9d,0x40
 675:	0f 84 1a 00 00 00    	je     0x695
 67b:	41 81 c3 12 00 00 00 	add    r11d,0x12
 682:	41 81 c2 f8 ff ff ff 	add    r10d,0xfffffff8
 689:	48 8b 3c 24          	mov    rdi,QWORD PTR [rsp]
 68d:	49 89 d4             	mov    r12,rdx
 690:	e9 26 00 00 00       	jmp    0x6bb
 695:	41 81 c2 f8 ff ff ff 	add    r10d,0xfffffff8
 69c:	49 89 d4             	mov    r12,rdx
 69f:	48 89 4c 24 28       	mov    QWORD PTR [rsp+0x28],rcx
 6a4:	e9 be f9 ff ff       	jmp    0x67
 6a9:	41 81 c3 12 00 00 00 	add    r11d,0x12
 6b0:	41 81 c2 fd ff ff ff 	add    r10d,0xfffffffd
 6b7:	48 8b 3c 24          	mov    rdi,QWORD PTR [rsp]
 6bb:	89 07                	mov    DWORD PTR [rdi],eax
 6bd:	44 89 47 0c          	mov    DWORD PTR [rdi+0xc],r8d
 6c1:	89 5f 18             	mov    DWORD PTR [rdi+0x18],ebx
 6c4:	44 89 67 1c          	mov    DWORD PTR [rdi+0x1c],r12d
 6c8:	48 8b 74 24 08       	mov    rsi,QWORD PTR [rsp+0x8]
 6cd:	44 89 0e             	mov    DWORD PTR [rsi],r9d
 6d0:	48 8b 54 24 10       	mov    rdx,QWORD PTR [rsp+0x10]
 6d5:	44 89 1a             	mov    DWORD PTR [rdx],r11d
 6d8:	48 8b 4c 24 18       	mov    rcx,QWORD PTR [rsp+0x18]
 6dd:	48 89 c8             	mov    rax,rcx
 6e0:	41 2b c2             	sub    eax,r10d
 6e3:	48 8b 5c 24 30       	mov    rbx,QWORD PTR [rsp+0x30]
 6e8:	4c 8b 64 24 38       	mov    r12,QWORD PTR [rsp+0x38]
 6ed:	4c 8b 6c 24 40       	mov    r13,QWORD PTR [rsp+0x40]
 6f2:	4c 8b 74 24 48       	mov    r14,QWORD PTR [rsp+0x48]
 6f7:	4c 8b 7c 24 50       	mov    r15,QWORD PTR [rsp+0x50]
 6fc:	48 83 c4 60          	add    rsp,0x60
 700:	48 89 ec             	mov    rsp,rbp
 703:	5d                   	pop    rbp
 704:	c3                   	ret
 705:	00 00                	add    BYTE PTR [rax],al
 707:	00 fc                	add    ah,bh
 709:	ff                   	(bad)
 70a:	ff                   	(bad)
 70b:	ff 00                	inc    DWORD PTR [rax]
 70d:	00 00                	add    BYTE PTR [rax],al
 70f:	00 ff                	add    bh,bh
 711:	ff                   	(bad)
 712:	ff                   	(bad)
 713:	ff 00                	inc    DWORD PTR [rax]
 715:	00 00                	add    BYTE PTR [rax],al
	...
