<?php

declare(strict_types=1);

namespace App;

final class Calculator
{
    public function add(int|float $left, int|float $right): int|float
    {
        return $left + $right;
    }

    public function subtract(int|float $left, int|float $right): int|float
    {
        return $left - $right;
    }

    public function multiply(int|float $left, int|float $right): int|float
    {
        return $left * $right;
    }
}
