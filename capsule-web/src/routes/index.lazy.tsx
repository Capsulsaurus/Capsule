import { createLazyFileRoute, Link } from '@tanstack/react-router';
import { FormattedMessage } from 'react-intl';
import { Button } from '@/components/ui/button';

export const Route = createLazyFileRoute('/')({
    component: Index,
});

function Index() {
    return (
        <div className="flex flex-col items-center justify-center min-h-[50vh] gap-4">
            <h1 className="text-4xl font-bold">
                <FormattedMessage id="home.title" />
            </h1>
            <p className="text-muted-foreground">
                <FormattedMessage id="home.subtitle" />
            </p>
            <div className="flex gap-4">
                <Link to="/dashboard">
                    <Button>
                        <FormattedMessage id="home.cta_dashboard" />
                    </Button>
                </Link>
                <Link to="/photos">
                    <Button variant="outline">
                        <FormattedMessage id="home.cta_photos" />
                    </Button>
                </Link>
            </div>
        </div>
    );
}
