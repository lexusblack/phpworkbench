<?php

declare(strict_types=1);

namespace App;

final class PsalmBroken
{
    public function undefinedVariable(): int
    {
        return $missing;
    }

    public function undefinedMethod(): string
    {
        $clean = new Clean();

        return $clean->doesNotExist();
    }
}
