.intel_syntax noprefix
.code32
.section .text
.global _start
.equ MODE, 0xf1df0
.equ TRANSITIONS, 0xf1df4
.equ INPUT_XY, 0x283620

# Loaded at F1800 in verified unused, writable Doslet BIOS RAM.
# The original main and secondary poll each lose only MOV reg, 11.
_start:
    pushfd
    pushad
    call select_mode
    popad
    popfd
    cmp byte ptr [MODE], 1
    je main_absolute
    mov esi, 11
    jmp 0x213fb0
main_absolute:
    mov eax, 3
    call driver
    call clamp_position
    mov ecx, ebp
    cmp eax, dword ptr [ecx+0x330]
    jne main_changed
    cmp byte ptr [ecx+0x334], 0
    je 0x213feb
main_changed:
    mov dword ptr [ecx+0x330], eax
    mov byte ptr [ecx+0x334], 0
    jmp 0x21408e

.org 0x100, 0x90
secondary:
    pushfd
    pushad
    call select_mode
    popad
    popfd
    cmp byte ptr [MODE], 1
    je secondary_absolute
    mov edx, 11
    jmp 0x214a5a
secondary_absolute:
    mov eax, 3
    call driver
    call clamp_position
    cmp eax, dword ptr [ecx+0x330]
    jne secondary_changed
    cmp byte ptr [ecx+0x334], 0
    je 0x214a91
secondary_changed:
    mov dword ptr [ecx+0x330], eax
    mov byte ptr [ecx+0x334], 0
    jmp 0x214b28

.org 0x200, 0x90
# Called with caller registers/flags saved. Unknown screens stay relative.
select_mode:
    xor ebx, ebx
    cmp dword ptr [0x2832c8], 0x2963a4
    jne selected
    cmp word ptr [0x2963b0], 1
    jne selected
    cmp word ptr [0x295a1c], 0
    jne selected
    inc ebx
selected:
    cmp bl, byte ptr [MODE]
    je selected_done
    # Align driver to game cursor, then drain accumulated relative motion.
    # Only transitions read 000Bh; stable city polling reads 0003h only.
    mov edx, dword ptr [INPUT_XY]
    mov eax, 4
    call driver
    mov eax, 11
    call driver
    mov byte ptr [MODE], bl
    inc dword ptr [TRANSITIONS]
selected_done:
    ret

.org 0x300, 0x90
# EAX=function, EDX=packed X/Y input for function 4.
# Return packed CX/DX in EAX, preserve all other registers and flags.
# Use the game's own protected/real-mode INT wrapper and two 28-byte REGS.
driver:
    pushfd
    pushad
    sub esp, 56
    cld
    xor eax, eax
    mov edi, esp
    mov ecx, 14
    rep stosd
    mov eax, dword ptr [esp+84]
    mov dword ptr [esp], eax
    mov eax, dword ptr [esp+76]
    mov word ptr [esp+8], ax
    shr eax, 16
    mov word ptr [esp+12], ax
    mov edx, esp
    lea ebx, [esp+28]
    mov eax, 0x33
    call 0x21213d
    movzx eax, word ptr [esp+36]
    movzx edx, word ptr [esp+40]
    shl edx, 16
    or eax, edx
    mov dword ptr [esp+84], eax
    add esp, 56
    popad
    popfd
    ret

.org 0x400, 0x90
# The selected screen is 640x480. Preserve ECX, EDX and the output pointer ESI.
clamp_position:
    cmp ax, 639
    jbe x_valid
    mov ax, 639
x_valid:
    ror eax, 16
    cmp ax, 479
    jbe y_valid
    mov ax, 479
y_valid:
    rol eax, 16
    ret

.org 0x5f0, 0
    .byte 2
    .byte 0, 0, 0
    .long 0
    .ascii "HABSv001"
