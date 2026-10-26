<?php

declare(strict_types=1);

namespace App;

final class Broken
{
    public function undefinedVariable(): int
    {
        return $missing;
    }

    public function wrongReturnType(): int
    {
        return 'not an int';
    }

    public function undefinedMethod(): string
    {
        $clean = new Clean();

        return $clean->doesNotExist();
    }
}
