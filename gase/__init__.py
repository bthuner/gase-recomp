"""
gase-recomp — a pedagogic static recompiler for Sega Genesis / Mega Drive.

Pipeline (one module per step, read them in this order):

    rom.py        load the ROM, parse the header and the vector table
    m68k/         decode 68000 machine code into Instruction objects
    analysis.py   find the code: functions, branches, call graph
    codegen.py    turn each function into C source code
    project.py    write a buildable project for the chosen platform
    cli.py        the `python -m gase` command line
"""

__version__ = "0.1.0"
