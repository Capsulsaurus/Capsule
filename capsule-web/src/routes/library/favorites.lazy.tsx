import { createLazyFileRoute } from '@tanstack/react-router';
import { FormattedMessage } from 'react-intl';

export const Route = createLazyFileRoute('/library/favorites')({
    component: RouteComponent,
});

function RouteComponent() {
    return (
        <div>
            <FormattedMessage id="library.favorites.placeholder" />
        </div>
    );
}
