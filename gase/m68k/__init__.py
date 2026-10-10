"""The Motorola 68000: operand model and instruction decoder."""

from .decoder import CONDITIONS, DecodeError, Instruction, decode
from .operands import Operand

__all__ = ["CONDITIONS", "DecodeError", "Instruction", "Operand", "decode"]
