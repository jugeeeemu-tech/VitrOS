ENTRY(kernel_main)

/* 仮想アドレス空間の基底 */
KERNEL_VMA = 0xFFFF800000000000;
KERNEL_LMA = 0x200000;

SECTIONS
{
    /* 仮想アドレスは高位、物理アドレスは低位 */
    . = KERNEL_VMA + KERNEL_LMA;

    .text ALIGN(4K) : AT(KERNEL_LMA)
    {
        *(.text .text.*)
        *(.ltext .ltext.*)
    }

    .rodata ALIGN(4K) : AT(ALIGN(LOADADDR(.text) + SIZEOF(.text), 4K))
    {
        *(.rodata .rodata.*)
        *(.lrodata .lrodata.*)
    }

    .eh_frame_hdr ALIGN(4K) : AT(ALIGN(LOADADDR(.rodata) + SIZEOF(.rodata), 4K))
    {
        *(.eh_frame_hdr)
    }

    .data ALIGN(4K) : AT(ALIGN(LOADADDR(.eh_frame_hdr) + SIZEOF(.eh_frame_hdr), 4K))
    {
        *(.got)
        *(.data .data.*)
        *(.ldata .ldata.*)
    }

    .bss ALIGN(4K) : AT(ALIGN(LOADADDR(.data) + SIZEOF(.data), 4K))
    {
        *(.bss .bss.*)
        *(.lbss .lbss.*)
        *(COMMON)
    }

    /DISCARD/ : { *(.eh_frame) *(.note .note.*) }
}
